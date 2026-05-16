#![allow(dead_code)]

use anyhow::{anyhow, Result};

use crate::memory::MemoryStore;

pub fn list_memory_topics(memory: Option<&MemoryStore>) -> Result<String> {
    let Some(memory) = memory else {
        return Ok("Memory is not available in this session.".to_string());
    };
    memory.list_topics()
}

pub fn read_memory_topic(memory: Option<&MemoryStore>, topic: &str) -> Result<String> {
    if topic.trim().is_empty() {
        return Err(anyhow!("read_memory_topic: missing topic"));
    }
    let Some(memory) = memory else {
        return Ok("Memory is not available in this session.".to_string());
    };
    memory.read_topic(topic)
}

pub fn search_memory_transcripts(
    memory: Option<&MemoryStore>,
    query: &str,
    case_sensitive: bool,
    max_matches: usize,
) -> Result<String> {
    if query.trim().is_empty() {
        return Err(anyhow!(
            "search_memory_transcripts: missing query; provide a specific identifier, path, symbol, or error code"
        ));
    }
    let Some(memory) = memory else {
        return Ok("Memory is not available in this session.".to_string());
    };
    memory.search_transcripts(query, case_sensitive, max_matches)
}
