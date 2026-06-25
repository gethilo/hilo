// Debounce logic — suppresses duplicate file events within a time window.
//
// The Debouncer tracks (path, event_type) -> last_fired timestamps and
// prevents rapid-fire duplicate triggers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::EventType;

pub struct Debouncer {
    /// Map from (path, event_type) to last_fired_timestamp.
    last_fired: HashMap<(PathBuf, EventType), Instant>,
    /// Debounce window duration.
    window: Duration,
}

impl Debouncer {
    pub fn new(window_ms: u64) -> Self {
        Self {
            last_fired: HashMap::new(),
            window: Duration::from_millis(window_ms),
        }
    }

    /// Returns true if the event should fire (window has elapsed since last
    /// fire for this file+type).  Updates last_fired on true, does NOT update
    /// on false (waits for full window).
    pub fn should_fire(&mut self, path: &Path, event_type: &EventType) -> bool {
        let key = (path.to_path_buf(), event_type.clone());
        let now = Instant::now();

        if let Some(last) = self.last_fired.get(&key) {
            if now.duration_since(*last) < self.window {
                return false; // Within window, suppress
            }
        }

        self.last_fired.insert(key, now);
        true
    }

    /// Per-file debounce: only fires when no event has been received for this
    /// specific file within the window, regardless of event type.
    pub fn should_fire_file(&mut self, path: &Path) -> bool {
        let now = Instant::now();
        let path_owned = path.to_path_buf();

        // Check if any (path, *) entry is within the window.
        let suppressed = self
            .last_fired
            .iter()
            .any(|((p, _), last)| p == &path_owned && now.duration_since(*last) < self.window);

        if suppressed {
            return false;
        }

        // Record this fire.  EventType::Create is used as the sentinel since
        // per-file debouncing ignores event type.
        self.last_fired.insert((path_owned, EventType::Create), now);
        true
    }
}
