use std::path::Path;
use hilo_triggers::{Debouncer, EventType, TriggerConfig, TriggerEngine};

#[test]
fn test_debouncer_should_fire_first_event() {
    let mut d = Debouncer::new(500);
    assert!(d.should_fire(Path::new("test.go"), &EventType::Write));
}

#[test]
fn test_debouncer_suppresses_rapid_events() {
    let mut d = Debouncer::new(500);
    assert!(d.should_fire(Path::new("test.go"), &EventType::Write));
    assert!(!d.should_fire(Path::new("test.go"), &EventType::Write));
}

#[test]
fn test_debouncer_per_file_isolation() {
    let mut d = Debouncer::new(500);
    assert!(d.should_fire(Path::new("a.go"), &EventType::Write));
    assert!(d.should_fire(Path::new("b.go"), &EventType::Write));
}

#[test]
fn test_debouncer_should_fire_file() {
    let mut d = Debouncer::new(500);
    assert!(d.should_fire_file(Path::new("test.go")));
    assert!(!d.should_fire_file(Path::new("test.go")));
}

#[test]
fn test_engine_creation() {
    let cfg = TriggerConfig::default();
    let _engine = TriggerEngine::new(vec![cfg], 500);
}

#[test]
fn test_parse_duration_ms() {
    assert_eq!(hilo_triggers::parse_duration_ms("500ms"), 500);
    assert_eq!(hilo_triggers::parse_duration_ms("2s"), 2000);
    assert_eq!(hilo_triggers::parse_duration_ms("30s"), 30000);
}
