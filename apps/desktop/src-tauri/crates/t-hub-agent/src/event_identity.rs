//! Credential-safe provider event identities.
//!
//! The digest input is limited to provider-owned identifiers and fixed domain
//! separators. Prompts, commands, tool arguments, output, paths, and other
//! content must never be passed to this module.

use sha2::{Digest, Sha256};

const IDENTITY_DOMAIN: &str = "t-hub.provider-event.v1";
const IDENTITY_PREFIX: &str = "provider-event:v1:";
const MAX_COMPONENT_BYTES: usize = 512;
const MAX_COMPONENTS: usize = 8;

/// Derive a stable opaque identity from exact provider identifiers.
///
/// Each UTF-8 component is encoded as an unsigned big-endian 32-bit byte
/// length followed by its exact bytes. Labels are sorted before hashing, so
/// caller iteration order cannot change the result. Missing, empty, oversized,
/// or excessive provider IDs fail closed to `None`, leaving the event
/// intentionally non-deduplicable.
pub fn derive(
    provider: &str,
    provider_event_kind: &str,
    provider_ids: &[(&str, Option<&str>)],
) -> Option<String> {
    if provider.is_empty()
        || provider_event_kind.is_empty()
        || provider.len() > MAX_COMPONENT_BYTES
        || provider_event_kind.len() > MAX_COMPONENT_BYTES
        || provider_ids.len() > MAX_COMPONENTS
    {
        return None;
    }

    let mut ids = provider_ids
        .iter()
        .filter_map(|(label, value)| value.map(|value| (*label, value)))
        .collect::<Vec<_>>();
    if ids.is_empty()
        || ids.iter().any(|(label, value)| {
            label.is_empty()
                || value.is_empty()
                || label.len() > MAX_COMPONENT_BYTES
                || value.len() > MAX_COMPONENT_BYTES
        })
    {
        return None;
    }
    ids.sort_unstable();

    let mut hasher = Sha256::new();
    update_component(&mut hasher, IDENTITY_DOMAIN)?;
    update_component(&mut hasher, provider)?;
    update_component(&mut hasher, provider_event_kind)?;
    for (label, value) in ids {
        update_component(&mut hasher, label)?;
        update_component(&mut hasher, value)?;
    }
    Some(format!("{IDENTITY_PREFIX}{:x}", hasher.finalize()))
}

fn update_component(hasher: &mut Sha256, value: &str) -> Option<()> {
    let bytes = value.as_bytes();
    let len = u32::try_from(bytes.len()).ok()?;
    hasher.update(len.to_be_bytes());
    hasher.update(bytes);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_and_component_order_independent() {
        let first = derive(
            "codex",
            "permission_requested",
            &[("turn_id", Some("turn-1")), ("request_id", Some("req-1"))],
        )
        .unwrap();
        let reordered = derive(
            "codex",
            "permission_requested",
            &[("request_id", Some("req-1")), ("turn_id", Some("turn-1"))],
        )
        .unwrap();
        assert_eq!(first, reordered);
        assert_eq!(first.len(), IDENTITY_PREFIX.len() + 64);
    }

    #[test]
    fn length_prefixes_prevent_component_boundary_aliases() {
        let left = derive(
            "codex",
            "turn_completed",
            &[("a", Some("bc")), ("d", Some("e"))],
        )
        .unwrap();
        let right = derive(
            "codex",
            "turn_completed",
            &[("a", Some("b")), ("c", Some("de"))],
        )
        .unwrap();
        assert_ne!(left, right);
    }

    #[test]
    fn missing_empty_and_oversized_provider_ids_are_non_deduplicable() {
        assert_eq!(derive("codex", "turn_started", &[]), None);
        assert_eq!(
            derive("codex", "turn_started", &[("turn_id", Some(""))]),
            None
        );
        let oversized = "x".repeat(MAX_COMPONENT_BYTES + 1);
        assert_eq!(
            derive("codex", "turn_started", &[("turn_id", Some(&oversized))]),
            None
        );
    }

    #[test]
    fn opaque_digest_never_contains_provider_or_content_canaries() {
        let provider_id = "provider-id-secret-canary";
        let content_canary = "prompt-tool-output-secret-canary";
        let identity = derive(
            "claude",
            "permission_requested",
            &[("event_id", Some(provider_id))],
        )
        .unwrap();
        assert!(!identity.contains(provider_id));
        assert!(!identity.contains(content_canary));
    }
}
