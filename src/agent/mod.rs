// Core agent loop: prompt → LLM → tool → repeat, with token budget tracking.

pub mod budget;
pub mod context;
