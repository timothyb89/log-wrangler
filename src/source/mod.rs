use std::collections::HashMap;

pub mod stdin;

pub struct RawLog {
    pub timestamp: jiff::Zoned,
    pub message: String,
    pub labels: HashMap<String, String>,
}
