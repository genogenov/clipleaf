use std::rc::Rc;

use crate::{daemon_client::ClientEvent, ui::main_window::MainWindow};

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
    pub fn apply(&mut self, u: ClientEvent, main: &Rc<MainWindow>) -> bool {
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
                main.set_status(&self.status_text(), true);
                true
            }
            ClientEvent::Disconnected => {
                if let ConnState::Disconnected = self.conn {
                    return false; // no change
                }
                self.conn = ConnState::Disconnected;
                main.set_status(&self.status_text(), false);
                true
            }
            ClientEvent::List { connection_id, list } => {
                if self.con_id != connection_id {
                    return false;
                }
                main.set_list(list);
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
}
