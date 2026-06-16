use dbgatlas_mcp::{McpServer, serve_stdio};
use std::io::{self, BufReader};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), dbgatlas_mcp::McpError> {
    let input = BufReader::new(io::stdin());
    let output = io::stdout();
    serve_stdio(McpServer::with_process_workers()?, input, output)
}
