/** @type {import('tailwindcss').Config} */
module.exports = {
  darkMode: 'class', // Use class-based dark mode
  content: [
    "./crates/dragonfly-server/templates/**/*.html", // Scan HTML templates
    "./crates/dragonfly-server/src/**/*.rs",       // Scan Rust files for any potential classes (optional but good practice)
  ],
  theme: {
    extend: {},
  },
  plugins: [],
} 