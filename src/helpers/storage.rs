use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use crate::config;

pub fn load_statuses() -> HashMap<String, String> {
    if let Ok(file) = File::open(config::STATUS_FILE) {
        let reader = BufReader::new(file);
        serde_json::from_reader(reader).unwrap_or_else(|_| HashMap::new())
    } else {
        HashMap::new()
    }
}

pub fn save_statuses(statuses: &HashMap<String, String>) {
    if let Ok(file) = File::create(config::STATUS_FILE) {
        let writer = BufWriter::new(file);
        let _ = serde_json::to_writer(writer, statuses);
    }
}
