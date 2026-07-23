//! Desktop adapter for the canonical Linux Preview listener inspector.

use super::endpoint::ListenerOwnership;

pub(crate) fn listener_ownership(port: u16) -> Result<Option<ListenerOwnership>, String> {
    t_hub_preview_runtime::listener_ownership(port).map(|ownership| {
        ownership.map(|ownership| ListenerOwnership {
            process_group_id: ownership.process_group_id,
            process_group_started_at: ownership.process_group_started_at,
        })
    })
}

#[cfg(test)]
pub(crate) fn process_group_identity_for_pid(pid: u32) -> Result<ListenerOwnership, String> {
    t_hub_preview_runtime::process_group_identity_for_pid(pid).map(|ownership| ListenerOwnership {
        process_group_id: ownership.process_group_id,
        process_group_started_at: ownership.process_group_started_at,
    })
}
