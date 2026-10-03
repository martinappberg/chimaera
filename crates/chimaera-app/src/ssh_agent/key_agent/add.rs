//! Public identity extraction from the fixed local software-key loader only.
//! Private fields remain borrowed from its zeroizing frame. This parser does
//! not authorize a relay, a signature, a socket, or an unbound agent request.
use super::super::{packet::Reader, selection::SelectionFailure};
use zeroize::Zeroizing;
type Result<T> = std::result::Result<T, SelectionFailure>;
fn string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}
fn bounded<'a>(reader: &mut Reader<'a>, max: usize) -> Result<&'a [u8]> {
    let value = reader.string().map_err(|_| SelectionFailure::Unavailable)?;
    if value.is_empty() || value.len() > max {
        return Err(SelectionFailure::Unavailable);
    }
    Ok(value)
}
pub(super) fn public_identity(frame: &Zeroizing<Vec<u8>>, lifetime: u32) -> Result<Vec<u8>> {
    if !(1..=180).contains(&lifetime) {
        return Err(SelectionFailure::Unavailable);
    }
    let mut r = Reader::new(frame).map_err(|_| SelectionFailure::Unavailable)?;
    // Fixed ssh-add always supplies -t. Certificates, FIDO/provider/card
    // commands, unconstrained adds and every other message type refuse.
    r.byte_is(25).map_err(|_| SelectionFailure::Unavailable)?;
    let algorithm = bounded(&mut r, 64)?;
    let mut public = Vec::new();
    string(&mut public, algorithm);
    match algorithm {
        b"ssh-ed25519" => {
            let key = bounded(&mut r, 32)?;
            let private = bounded(&mut r, 64)?;
            if key.len() != 32 || private.len() != 64 || private[32..] != *key {
                return Err(SelectionFailure::Unavailable);
            }
            string(&mut public, key);
        }
        b"ssh-rsa" => {
            let modulus = bounded(&mut r, 4096)?;
            let exponent = bounded(&mut r, 16)?;
            for _ in 0..4 {
                bounded(&mut r, 4096)?;
            }
            // Agent private serialization is n,e,d,iqmp,p,q; public is e,n.
            string(&mut public, exponent);
            string(&mut public, modulus);
        }
        b"ecdsa-sha2-nistp256" | b"ecdsa-sha2-nistp384" | b"ecdsa-sha2-nistp521" => {
            let curve = bounded(&mut r, 8)?;
            if algorithm != [b"ecdsa-sha2-".as_slice(), curve].concat() {
                return Err(SelectionFailure::Unavailable);
            }
            let key = bounded(&mut r, 133)?;
            bounded(&mut r, 67)?;
            string(&mut public, curve);
            string(&mut public, key);
        }
        _ => return Err(SelectionFailure::UnsupportedConfiguration),
    }
    let comment = r.string().map_err(|_| SelectionFailure::Unavailable)?;
    if comment.len() > 1024 || comment.contains(&0) {
        return Err(SelectionFailure::Unavailable);
    }
    r.byte_is(1).map_err(|_| SelectionFailure::Unavailable)?;
    if r.u32().map_err(|_| SelectionFailure::Unavailable)? != lifetime {
        return Err(SelectionFailure::Unavailable);
    }
    r.end().map_err(|_| SelectionFailure::Unavailable)?;
    let parsed =
        ssh_key::PublicKey::from_bytes(&public).map_err(|_| SelectionFailure::Unavailable)?;
    if parsed
        .to_bytes()
        .map_err(|_| SelectionFailure::Unavailable)?
        != public
    {
        return Err(SelectionFailure::Unavailable);
    }
    Ok(public)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn add() -> Zeroizing<Vec<u8>> {
        use ssh_key::{
            private::{Ed25519Keypair, KeypairData},
            PrivateKey,
        };
        let private = PrivateKey::new(
            KeypairData::Ed25519(Ed25519Keypair::from_seed(&[42; 32])),
            "",
        )
        .unwrap();
        let public = private.public_key().to_bytes().unwrap();
        let mut reader = Reader::new(&public).unwrap_or_else(|_| panic!("synthetic public key"));
        let algorithm = reader
            .string()
            .unwrap_or_else(|_| panic!("synthetic algorithm"));
        let key = reader
            .string()
            .unwrap_or_else(|_| panic!("synthetic public field"));
        assert!(reader.end().is_ok());
        let mut frame = Zeroizing::new(vec![25]);
        string(&mut frame, algorithm);
        string(&mut frame, key);
        let mut secret = Zeroizing::new(vec![42; 32]);
        secret.extend_from_slice(key);
        string(&mut frame, &secret);
        string(&mut frame, b"synthetic comment");
        frame.push(1);
        frame.extend_from_slice(&20u32.to_be_bytes());
        frame
    }
    #[test]
    fn only_complete_software_add_with_exact_lifetime_yields_public_bytes() {
        let frame = add();
        let public = public_identity(&frame, 20).unwrap();
        assert_eq!(
            ssh_key::PublicKey::from_bytes(&public).unwrap().algorithm(),
            ssh_key::Algorithm::Ed25519
        );
        for n in 0..frame.len() {
            assert!(public_identity(&Zeroizing::new(frame[..n].to_vec()), 20).is_err());
        }
        for lifetime in [0, 1, 19, 21, 181] {
            assert!(public_identity(&frame, lifetime).is_err());
        }
        for kind in [11, 13, 17, 18, 19, 22, 26, 27] {
            let mut invalid = add();
            invalid[0] = kind;
            assert!(public_identity(&invalid, 20).is_err());
        }
        let mut extra = add();
        extra.push(0);
        assert!(public_identity(&extra, 20).is_err());
        let mut mismatched = add();
        mismatched[25] ^= 1;
        assert!(public_identity(&mismatched, 20).is_err());
    }
    #[test]
    fn provider_certificate_unknown_and_oversized_fields_never_extract_private_values() {
        for algorithm in [
            "sk-ssh-ed25519@openssh.com",
            "ssh-ed25519-cert-v01@openssh.com",
            "ssh-dss",
            "unknown",
        ] {
            let mut frame = Zeroizing::new(vec![25]);
            string(&mut frame, algorithm.as_bytes());
            assert!(matches!(
                public_identity(&frame, 20),
                Err(SelectionFailure::UnsupportedConfiguration)
            ));
        }
        let mut frame = Zeroizing::new(vec![25]);
        string(&mut frame, b"ssh-ed25519");
        frame.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(public_identity(&frame, 20).is_err());
        assert!(public_identity(
            &Zeroizing::new(vec![0; chimaera_link::SSH_AUTH_PACKET_MAX + 1]),
            20
        )
        .is_err());
    }
}
