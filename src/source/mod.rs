use std::collections::HashMap;



pub struct RawLog {
    pub timestamp: jiff::Zoned,
    pub message: String,
    pub labels: HashMap<String, String>,
}
