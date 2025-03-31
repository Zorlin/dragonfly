#[derive(Parser, Debug)]
pub struct ServerArgs {
    /// Run in foreground instead of as a service
    #[arg(short, long)]
    pub foreground: bool,
} 