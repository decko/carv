// Core agent loop: prompt → LLM → tool → repeat, with token budget tracking.

#[path = "loop.rs"]
pub mod agent_loop;
pub mod budget;
pub mod context;
