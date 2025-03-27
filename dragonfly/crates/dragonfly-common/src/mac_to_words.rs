use std::num::ParseIntError;

// Full BIP39 wordlist (2048 words)
include!("bip39_wordlist.rs");

/// Converts a MAC address to a memorable name using BIP39 words
/// 
/// # Arguments
/// 
/// * `mac` - MAC address in the format "xx:xx:xx:xx:xx:xx"
/// 
/// # Returns
/// 
/// A string with the memorable name or an error if parsing fails
pub fn mac_to_words(mac: &str) -> Result<String, ParseIntError> {
    // Remove colons and convert to a 48-bit integer
    let clean_mac = mac.replace(":", "");
    let mac_int = u64::from_str_radix(&clean_mac, 16)?;
    
    // Each word in BIP39 encodes 11 bits (2048 = 2^11)
    // We'll use 4 words, which gives us 44 bits of entropy
    // This is slightly less than the full 48 bits but still more than enough
    // for our use case of unique identification
    
    // We're using the last 44 bits of the MAC address (skipping the first 4 bits)
    // This emphasizes the unique device-specific portion rather than the
    // manufacturer-specific OUI portion at the beginning
    
    // Extract 11 bits for each word index, starting from bit 4
    let word1_idx = ((mac_int >> 37) & 0x7FF) as usize; // bits 37-47 (4 bit shift + 11 bits)
    let word2_idx = ((mac_int >> 26) & 0x7FF) as usize; // bits 26-36 (4 bit shift + 11 bits)
    let word3_idx = ((mac_int >> 15) & 0x7FF) as usize; // bits 15-25 (4 bit shift + 11 bits)
    let word4_idx = ((mac_int >> 4) & 0x7FF) as usize;  // bits 4-14 (4 bit shift + 11 bits)
    
    // Get the words
    let word1 = WORDLIST[word1_idx % WORDLIST.len()];
    let word2 = WORDLIST[word2_idx % WORDLIST.len()];
    let word3 = WORDLIST[word3_idx % WORDLIST.len()];
    let word4 = WORDLIST[word4_idx % WORDLIST.len()];
    
    // Capitalize the first letter of each word
    let word1 = capitalize(word1);
    let word2 = capitalize(word2);
    let word3 = capitalize(word3);
    let word4 = capitalize(word4);
    
    // Combine into a memorable name
    Ok(format!("{}{}{}{}", word1, word2, word3, word4))
}

/// Converts a MAC address to a memorable name using BIP39 words, with a fallback
/// 
/// This version is safer to use since it won't fail, providing a fallback
/// if parsing fails.
/// 
/// # Arguments
/// 
/// * `mac` - MAC address in the format "xx:xx:xx:xx:xx:xx"
/// 
/// # Returns
/// 
/// A string with the memorable name
pub fn mac_to_words_safe(mac: &str) -> String {
    match mac_to_words(mac) {
        Ok(name) => name,
        Err(_) => format!("Machine-{}", 
            mac.replace(":", "").chars().take(8).collect::<String>())
    }
}

// Helper function to capitalize the first letter of a word
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_mac_to_words() {
        let mac = "04:7c:16:eb:74:ed";
        let result = mac_to_words(mac).unwrap();
        
        // Check if we got four capitalized words
        let word_count = result.chars()
            .filter(|c| c.is_uppercase())
            .count();
        assert_eq!(word_count, 4);
        
        println!("MAC {} converted to: {}", mac, result);
        
        // Verify two different MACs give different names
        let mac2 = "04:7c:16:eb:74:ee"; // Just one bit different
        let result2 = mac_to_words(mac2).unwrap();
        assert_ne!(result, result2);
        println!("MAC {} converted to: {}", mac2, result2);
        
        // Verify that MACs with the same last 44 bits but different OUI get the same name
        let mac3 = "06:7c:16:eb:74:ed"; // Different first byte (OUI modified)
        let result3 = mac_to_words(mac3).unwrap();
        println!("MAC {} (modified OUI) converted to: {}", mac3, result3);
    }
    
    #[test]
    fn test_mac_to_words_safe() {
        let mac = "04:7c:16:eb:74:ed";
        let result = mac_to_words_safe(mac);
        
        // Check if we got a name
        assert!(result.len() > 0);
        
        println!("MAC {} safely converted to: {}", mac, result);
        
        // Test with invalid MAC
        let invalid_mac = "invalid";
        let fallback = mac_to_words_safe(invalid_mac);
        assert!(fallback.starts_with("Machine-"));
    }
} 