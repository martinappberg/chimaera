use super::{Failure, Signature, SSH_AUTH_PACKET_MAX};

pub(super) struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Result<Self, Failure> {
        if bytes.len() > SSH_AUTH_PACKET_MAX {
            return Err(Failure::InvalidRequest);
        }
        Ok(Self(bytes))
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], Failure> {
        let value = self.0.get(..n).ok_or(Failure::InvalidRequest)?;
        self.0 = &self.0[n..];
        Ok(value)
    }
    pub(super) fn u32(&mut self) -> Result<u32, Failure> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| Failure::InvalidRequest)?,
        ))
    }
    pub(super) fn string(&mut self) -> Result<&'a [u8], Failure> {
        let length = self.u32()? as usize;
        self.take(length)
    }
    pub(super) fn string_is(&mut self, expected: &[u8]) -> Result<(), Failure> {
        if self.string()? != expected {
            return Err(Failure::InvalidRequest);
        }
        Ok(())
    }
    pub(super) fn byte_is(&mut self, expected: u8) -> Result<(), Failure> {
        if self.take(1)? != [expected] {
            return Err(Failure::InvalidRequest);
        }
        Ok(())
    }
    pub(super) fn end(self) -> Result<(), Failure> {
        if !self.0.is_empty() {
            return Err(Failure::InvalidRequest);
        }
        Ok(())
    }
}
pub(super) fn signature(bytes: &[u8]) -> Result<Signature, Failure> {
    // ssh-key's Signature::try_from decodes a prefix. Enforce a complete
    // canonical wire object here, including security-key flags/counter.
    let mut r = Reader::new(bytes)?;
    let algorithm = r.string()?;
    r.string()?;
    if matches!(
        algorithm,
        b"sk-ssh-ed25519@openssh.com" | b"sk-ecdsa-sha2-nistp256@openssh.com"
    ) {
        r.take(5)?;
    }
    r.end()?;
    Signature::try_from(bytes).map_err(|_| Failure::InvalidRequest)
}
