//! Lists every output device with the configurations it supports, and what
//! the engine would open it with for separate headphones.
//!
//! `cargo run -p rbl-deck --example outputs`
#![allow(clippy::print_stdout, reason = "a diagnostic tool prints what it finds")]

use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else {
        println!("no output devices");
        return;
    };
    for device in devices {
        println!("{device}");
        match device.default_output_config() {
            Ok(config) => println!("  default: {} ch, {} Hz, {}", config.channels(), config.sample_rate(), config.sample_format()),
            Err(e) => println!("  default: {e}"),
        }
        if let Ok(ranges) = device.supported_output_configs() {
            for range in ranges {
                println!(
                    "  supports: {} ch, {}-{} Hz, {}",
                    range.channels(),
                    range.min_sample_rate(),
                    range.max_sample_rate(),
                    range.sample_format()
                );
            }
        }
    }
}
