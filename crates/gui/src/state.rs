use crate::daemon_client::ClientEvent;

#[derive(PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected { daemon: Option<String> },
    Disconnected,
}
pub struct AppState {
    pub conn: ConnState,
    pub con_id: u64,
}
impl AppState {
    pub fn apply(&mut self, u: ClientEvent) -> bool {
        match u {
            ClientEvent::Connected { connection_id, daemon } => {
                if let ConnState::Connected { daemon: prev_daemon } = &self.conn
                    && prev_daemon == &daemon
                    && self.con_id == connection_id
                {
                    return false; // no change
                }
                self.conn = ConnState::Connected { daemon };
                self.con_id = connection_id;
                true
            }
            ClientEvent::Disconnected => {
                if let ConnState::Disconnected = self.conn {
                    return false; // no change
                }
                self.conn = ConnState::Disconnected;
                true
            }
        }
    }
    pub fn status_text(&self) -> String {
        match &self.conn {
            ConnState::Connecting => "Connecting to daemon…".to_string(),
            ConnState::Connected { daemon } => {
                if let Some(d) = daemon {
                    format!("Connected to daemon: {d}")
                } else {
                    "Connected to daemon".to_string()
                }
            }
            ConnState::Disconnected => "Disconnected from daemon".to_string(),
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.conn, ConnState::Connected { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(connection_id: u64, daemon: Option<&str>) -> ClientEvent {
        ClientEvent::Connected {
            connection_id,
            daemon: daemon.map(str::to_string),
        }
    }

    fn connecting() -> AppState {
        AppState {
            conn: ConnState::Connecting,
            con_id: 0,
        }
    }

    #[test]
    fn apply_reports_only_real_changes() {
        let mut s = connecting();
        assert!(s.apply(ClientEvent::Disconnected), "Connecting → Disconnected");
        assert!(!s.apply(ClientEvent::Disconnected), "repeated Disconnected");

        assert!(s.apply(connected(1, Some("d 1.0"))), "Disconnected → Connected");
        assert!(!s.apply(connected(1, Some("d 1.0"))), "same connection again");
        assert!(s.apply(connected(2, Some("d 1.0"))), "a new connection id is a change");
        assert!(s.apply(connected(2, None)), "a different daemon name is a change");
        assert!(s.apply(ClientEvent::Disconnected), "Connected → Disconnected");
    }

    #[test]
    fn status_text_and_is_connected_per_state() {
        let mut s = connecting();
        assert_eq!(s.status_text(), "Connecting to daemon…");
        assert!(!s.is_connected());

        s.apply(connected(1, Some("clipleafd 0.1.0")));
        assert_eq!(s.status_text(), "Connected to daemon: clipleafd 0.1.0");
        assert!(s.is_connected());

        s.apply(connected(2, None));
        assert_eq!(s.status_text(), "Connected to daemon");
        assert!(s.is_connected());

        s.apply(ClientEvent::Disconnected);
        assert_eq!(s.status_text(), "Disconnected from daemon");
        assert!(!s.is_connected());
    }
}
