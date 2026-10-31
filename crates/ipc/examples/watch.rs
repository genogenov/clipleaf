use std::time::Duration;

use clipleaf_core::{log_error, log_info};
use clipleaf_ipc::{client::Client, default_socket_path};

pub fn main() {
    let mut client = Client::connect_handshake(&default_socket_path().unwrap(), 4096, 4096, Some(Duration::from_secs(1))).unwrap();

    loop {
        match client.next_event(None) {
            Ok(Some(event)) => {
                log_info!("Got event: {event:?}");
            }
            Ok(None) => {
                log_info!("Wait for event timed out");
            }
            Err(e) => {
                log_error!("Error: {e}");
                break;
            }
        }
    }
}
