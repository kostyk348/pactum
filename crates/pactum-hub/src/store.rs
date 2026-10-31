//! Персистентность состояния хаба: один JSON-файл, переживает рестарт.
//! Очередь ретраев в нём же — потерять недоставленное хаб не имеет права.

use std::path::Path;

use crate::types::HubState;

/// Сохранить состояние (атомарно: во временный файл, затем rename).
pub fn save_state(state: &HubState, path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(state)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Загрузить состояние; отсутствующий файл — чистое состояние.
pub fn load_state(path: &Path) -> Result<HubState, std::io::Error> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(HubState::new()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SubState;

    #[test]
    fn roundtrip_preserves_subscriptions_and_queue() {
        let dir = std::env::temp_dir().join(format!("pactum-hub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hub.json");
        let mut state = HubState::new();
        state
            .subscribe("t", "http://a/cb", Some("sec"), None, 10)
            .unwrap();
        let sub_id = state.subscriptions[0].id.clone();
        state.mark_denied(&sub_id, 11);
        state.pending.push(crate::types::PendingDelivery {
            sub_id: state.subscriptions[0].id.clone(),
            topic: "t".into(),
            body_b64: crate::types::b64_encode(b"payload"),
            attempts: 3,
            next_at: 100,
        });
        save_state(&state, &path).unwrap();
        let back = load_state(&path).unwrap();
        assert_eq!(back.subscriptions.len(), 1);
        assert_eq!(back.subscriptions[0].state, SubState::Denied);
        assert_eq!(back.pending.len(), 1);
        assert_eq!(
            crate::types::b64_decode(&back.pending[0].body_b64).unwrap(),
            b"payload"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_fresh_state() {
        let state = load_state(Path::new("/tmp/нет-такого-файла-hub.json")).unwrap();
        assert!(state.subscriptions.is_empty());
    }
}
