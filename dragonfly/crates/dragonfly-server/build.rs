use std::process::Command;

fn main() {
    // Copy templates from the main dragonfly app
    Command::new("cp")
        .args(&["-r", "../../templates", "."])
        .status()
        .expect("Failed to copy templates");
    
    // Copy static files from the main dragonfly app
    Command::new("cp")
        .args(&["-r", "../../static", "."])
        .status()
        .expect("Failed to copy static files");
        
    println!("cargo:rerun-if-changed=../../templates");
    println!("cargo:rerun-if-changed=../../static");
} 