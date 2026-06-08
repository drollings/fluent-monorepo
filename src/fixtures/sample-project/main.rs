# Sample Rust file for AST parsing tests

pub fn greet(name: &str) -> String {
    format!("Hello, {name}")
}

pub struct Config {
    pub port: u16,
    pub host: String,
}
