use std::process::Command;
use std::path::Path;
use std::env;
use std::fs;
 
fn main() {
    // Rerun build script if build.rs, input CSS, or templates change
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/input.css");
    println!("cargo:rerun-if-changed=templates"); 
    
    // Define paths relative to the crate root (where build.rs is)
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let input_css_path = Path::new(&crate_dir).join("src/input.css");
    let output_css_path = Path::new(&crate_dir).join("static/css/tailwind.css");
    
    // Ensure output directory exists
    if let Some(parent) = output_css_path.parent() {
        fs::create_dir_all(parent).expect("Failed to create CSS output directory");
    }
    
    // Path to tailwind config relative to workspace root
    let workspace_root = Path::new(&crate_dir).parent().unwrap().parent().unwrap();
    let config_path = workspace_root.join("tailwind.config.js");
    
    // Path to locally installed Tailwind CLI binary
    let tailwind_bin = workspace_root.join("node_modules/.bin/tailwindcss");
    
    eprintln!("Building Tailwind CSS using local binary");
    eprintln!("Input: {}", input_css_path.display());
    eprintln!("Output: {}", output_css_path.display());
    eprintln!("Config: {}", config_path.display());
    
    // Run locally installed Tailwind CLI
    let output = Command::new(tailwind_bin)
        .current_dir(workspace_root)
        .arg("-i")
        .arg(input_css_path.to_str().unwrap())
        .arg("-o")
        .arg(output_css_path.to_str().unwrap())
        .arg("-c")
        .arg(config_path.to_str().unwrap())
        .output();
        
    match output {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                
                eprintln!("Failed to build Tailwind CSS:");
                eprintln!("STDOUT: {}", stdout);
                eprintln!("STDERR: {}", stderr);
                
                panic!("Tailwind CSS build failed with status: {}", output.status);
            } else {
                let stdout = String::from_utf8_lossy(&output.stdout);
                eprintln!("Tailwind CSS build successful!");
                if !stdout.is_empty() {
                    eprintln!("Output: {}", stdout);
                }
                
                // Verify the output file exists and has reasonable content
                if let Ok(metadata) = fs::metadata(&output_css_path) {
                    let size = metadata.len();
                    eprintln!("Generated CSS file size: {} bytes", size);
                    if size < 1000 {
                        eprintln!("Warning: CSS file seems suspiciously small");
                    }
                }
            }
        },
        Err(e) => {
            eprintln!("Failed to execute Tailwind CLI: {}", e);
            panic!("Failed to execute Tailwind CLI: {}", e);
        }
    }
} 