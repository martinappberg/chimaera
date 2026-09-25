//! Knowledge: a read-only view over what agents record, through providers
//! (mycelium today) plus the guidance and memory files agents already keep.
//! Chimaera never writes knowledge (plan decision 3).

use std::sync::Arc;

use crate::timeline::Recorded;
use crate::AppState;

/// What the workspace's knowledge provider gained since the last check,
/// attributed to the episode that just ended in `sid` — only when that
/// attribution is unambiguous (see the provider notes). None until a
/// structured provider is active.
pub(crate) async fn recorded_since_last_check(
    _state: &Arc<AppState>,
    _ws: &str,
    _sid: &str,
) -> Option<Recorded> {
    None
}
