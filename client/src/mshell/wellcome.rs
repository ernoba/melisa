use colored::*;
use sysinfo::System;
use std::io::{self, Write};

pub fn display_melisa_banner() {
    let mut sys = System::new_all();
    sys.refresh_all();

    // 1. Clear Screen instan
    print!("{}[2J{}[1;1H", 27 as char, 27 as char);

    // 2. Logo Sederhana (Clean ASCII)
    println!("{}", "MELISA SYSTEM".cyan().bold());
    println!("{}", "Management Environment Linux Sandbox | v0.1.4".bright_black());
    println!("{}", "----------------------------------------------------".bright_black());
    io::stdout().flush().unwrap();
}