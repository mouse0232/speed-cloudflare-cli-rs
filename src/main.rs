use anyhow::Result;
use bytes::Bytes;
use clap::{Arg, Command};
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use rand::Rng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::{Duration, Instant};
use tokio::time::sleep;

const CLOUDFLARE_DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down";
const CLOUDFLARE_UPLOAD_URL: &str = "https://speed.cloudflare.com/__up";

#[derive(Debug, Clone)]
struct SpeedTestConfig {
    download_bytes: Vec<usize>,
    upload_bytes: Vec<usize>,
    latency_tests: usize,
    packet_loss_tests: usize, // 新增：丢包率测试次数
    ip_version: IpVersion,
}

#[derive(Debug, Clone)]
enum IpVersion {
    V4,
    V6,
    Auto,
}

impl Default for SpeedTestConfig {
    fn default() -> Self {
        Self {
            download_bytes: vec![100_000, 1_000_000, 10_000_000, 25_000_000, 100_000_000],
            upload_bytes: vec![1_000, 10_000, 100_000, 1_000_000, 10_000_000, 25_000_000],
            latency_tests: 20,
            packet_loss_tests: 50, // 默认测试50次
            ip_version: IpVersion::Auto,
        }
    }
}

impl FromStr for IpVersion {
    type Err = ();
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "4" => Ok(IpVersion::V4),
            "6" => Ok(IpVersion::V6),
            "auto" => Ok(IpVersion::Auto),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SpeedTestResults {
    server_location: String,
    your_ip: String,
    latency_ms: f64,
    packet_loss_pct: f64, // 新增：丢包率百分比
    speed_100kb: f64,
    speed_1mb: f64,
    speed_10mb: f64,
    speed_25mb: f64,
    speed_100mb: f64,
    download_mbps: f64,
    upload_mbps: f64,
    jitter_ms: f64,
}

struct SpeedTest {
    client: Client,
    config: SpeedTestConfig,
}

impl SpeedTest {
    fn new(config: SpeedTestConfig) -> Self {
        let mut client_builder = Client::builder()
            .timeout(Duration::from_secs(30));
        
        // 根据配置设置IP版本
        match config.ip_version {
            IpVersion::V4 => {
                client_builder = client_builder.local_address(Some("0.0.0.0".parse().unwrap()));
            },
            IpVersion::V6 => {
                client_builder = client_builder.local_address(Some("::".parse().unwrap()));
            },
            IpVersion::Auto => {
                // 使用默认行为，不特别指定IP版本
            }
        }
        
        let client = client_builder.build().unwrap();
        
        Self { client, config }
    }

    async fn run(&self) -> Result<SpeedTestResults> {
    println!("{}", "Starting Cloudflare Speed Test...".bright_cyan());
    
    let server_info = self.get_server_info().await?;
    let latency_results = self.measure_latency().await?;
    let packet_loss_pct = self.measure_packet_loss().await?; // 新增
    let download_speeds = self.measure_download_detailed().await?;
    let upload_speed = self.measure_upload().await?;
    
    let latency_ms = latency_results.iter().sum::<f64>() / latency_results.len() as f64;
    let jitter_ms = self.calculate_jitter(&latency_results);
    
    Ok(SpeedTestResults {
        server_location: server_info.0,
        your_ip: server_info.1,
        latency_ms,
        packet_loss_pct, // 新增
        speed_100kb: download_speeds.get(&100_000).copied().unwrap_or(0.0),
        speed_1mb: download_speeds.get(&1_000_000).copied().unwrap_or(0.0),
        speed_10mb: download_speeds.get(&10_000_000).copied().unwrap_or(0.0),
        speed_25mb: download_speeds.get(&25_000_000).copied().unwrap_or(0.0),
        speed_100mb: download_speeds.get(&100_000_000).copied().unwrap_or(0.0),
        download_mbps: download_speeds.values().max_by(|a, b| a.partial_cmp(b).unwrap()).copied().unwrap_or(0.0),
        upload_mbps: upload_speed,
        jitter_ms,
    })
}

    async fn measure_latency(&self) -> Result<Vec<f64>> {
        println!("{}", "Measuring latency...".yellow());
        let pb = ProgressBar::new(self.config.latency_tests as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"));

        let mut latencies = Vec::new();
        
        for _ in 0..self.config.latency_tests {
            let start = Instant::now();
            let response = self.client
                .get(CLOUDFLARE_DOWNLOAD_URL)
                .query(&[("bytes", "1")])
                .send()
                .await?;
            
            if response.status().is_success() {
                let latency = start.elapsed().as_millis() as f64;
                latencies.push(latency);
            }
            
            pb.inc(1);
            sleep(Duration::from_millis(50)).await;
        }
        
        pb.finish_with_message("Latency measurement complete");
        Ok(latencies)
    }

    async fn get_server_info(&self) -> Result<(String, String)> {
        let response = self.client
            .get("https://www.cloudflare.com/cdn-cgi/trace")
            .send()
            .await?;
        
        let text = response.text().await?;
        let mut server_location = "Unknown".to_string();
        let mut your_ip = "Unknown".to_string();
        
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                match key {
                    "colo" => server_location = value.to_string(),
                    "ip" => your_ip = value.to_string(),
                    _ => {}
                }
            }
        }
        
        Ok((server_location, your_ip))
    }

    async fn measure_download_detailed(&self) -> Result<HashMap<usize, f64>> {
        println!("{}", "Measuring download speed...".yellow());
        let pb = ProgressBar::new(self.config.download_bytes.len() as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"));

        let mut speeds = HashMap::new();
        
        for &bytes in &self.config.download_bytes {
            let start = Instant::now();
            let response = self.client
                .get(CLOUDFLARE_DOWNLOAD_URL)
                .query(&[("bytes", bytes.to_string())])
                .send()
                .await?;
            
            if response.status().is_success() {
                let _content = response.bytes().await?;
                let duration = start.elapsed();
                let mbps = (bytes as f64 * 8.0) / (duration.as_secs_f64() * 1_000_000.0);
                speeds.insert(bytes, mbps);
            }
            
            pb.inc(1);
        }
        
        pb.finish_with_message("Download speed measurement complete");
        Ok(speeds)
    }

    async fn measure_upload(&self) -> Result<f64> {
        println!("{}", "Measuring upload speed...".yellow());
        let pb = ProgressBar::new(self.config.upload_bytes.len() as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"));

        let mut speeds = Vec::new();
        
        for &bytes in &self.config.upload_bytes {
            let data = self.generate_random_data(bytes);
            let start = Instant::now();
            
            let response = self.client
                .post(CLOUDFLARE_UPLOAD_URL)
                .body(data)
                .send()
                .await?;
            
            if response.status().is_success() {
                let duration = start.elapsed();
                let mbps = (bytes as f64 * 8.0) / (duration.as_secs_f64() * 1_000_000.0);
                speeds.push(mbps);
            }
            
            pb.inc(1);
        }
        
        pb.finish_with_message("Upload speed measurement complete");
        
        speeds.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p90_index = (speeds.len() as f64 * 0.9) as usize;
        Ok(speeds.get(p90_index).copied().unwrap_or(0.0))
    }

    fn generate_random_data(&self, size: usize) -> Bytes {
        let mut rng = rand::thread_rng();
        let data: Vec<u8> = (0..size).map(|_| rng.r#gen::<u8>()).collect(); // 修复关键字问题
        Bytes::from(data)
    }

    fn calculate_jitter(&self, latencies: &[f64]) -> f64 {
        if latencies.len() < 2 {
            return 0.0;
        }
        
        let mean = latencies.iter().sum::<f64>() / latencies.len() as f64;
        let variance = latencies.iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f64>() / latencies.len() as f64;
        variance.sqrt()
    }

    fn display_results(&self, results: &SpeedTestResults) {
    println!("\n{}", "═".repeat(60).bright_cyan());
    println!("{}", "           CLOUDFLARE SPEED TEST RESULTS".bright_cyan().bold());
    println!("{}", "═".repeat(60).bright_cyan());
    
    println!(
        "{:<20} {}",
        "Server location:".bright_white(),
        results.server_location.bright_green()
    );
    
    // 修复 IP 隐藏显示逻辑
    let masked_ip = if results.your_ip.contains('.') {
        // IPv4: 显示前三位，最后一位用***代替
        let parts: Vec<&str> = results.your_ip.split('.').collect();
        if parts.len() == 4 {
            format!("{}.{}.{}.***", parts[0], parts[1], parts[2])
        } else {
            "***.***.***.***".to_string()
        }
    } else {
        // IPv6: 显示前两段，其余用****代替
        let parts: Vec<&str> = results.your_ip.split(':').collect();
        if parts.len() > 2 {
            format!("{}:{}:****", parts[0], parts[1])
        } else {
            "****:****:****".to_string()
        }
    };
    
    println!(
        "{:<20} {}",
        "Your IP:".bright_white(),
        masked_ip.bright_green()
    );
    // ... rest of the method
}
    
    println!(
        "{:<20} {:.2} {}",
        "Latency:".bright_white(),
        results.latency_ms,
        "ms".bright_yellow()
    );
    
    // 新增：显示丢包率
    println!(
        "{:<20} {:.2} {}",
        "Packet Loss:".bright_white(),
        results.packet_loss_pct,
        "%".bright_red()
    );
    
        
        if results.speed_100kb > 0.0 {
            println!(
                "{:<20} {:.2} {}",
                "100kB speed:".bright_white(),
                results.speed_100kb,
                "Mbps".bright_cyan()
            );
        }
        
        if results.speed_1mb > 0.0 {
            println!(
                "{:<20} {:.2} {}",
                "1MB speed:".bright_white(),
                results.speed_1mb,
                "Mbps".bright_cyan()
            );
        }
        
        if results.speed_10mb > 0.0 {
            println!(
                "{:<20} {:.2} {}",
                "10MB speed:".bright_white(),
                results.speed_10mb,
                "Mbps".bright_cyan()
            );
        }
        
        if results.speed_25mb > 0.0 {
            println!(
                "{:<20} {:.2} {}",
                "25MB speed:".bright_white(),
                results.speed_25mb,
                "Mbps".bright_cyan()
            );
        }
        
        if results.speed_100mb > 0.0 {
            println!(
                "{:<20} {:.2} {}",
                "100MB speed:".bright_white(),
                results.speed_100mb,
                "Mbps".bright_cyan()
            );
        }
        
        println!(
            "{:<20} {:.2} {}",
            "Download speed:".bright_white(),
            results.download_mbps,
            "Mbps".bright_green()
        );
        
        println!(
            "{:<20} {:.2} {}",
            "Upload speed:".bright_white(),
            results.upload_mbps,
            "Mbps".bright_green()
        );
        
        println!("{}", "═".repeat(60).bright_cyan());
        
        self.display_quality_rating(results);
    }

    fn display_quality_rating(&self, results: &SpeedTestResults) {
    let download_rating = self.get_speed_rating(results.download_mbps);
    let latency_rating = self.get_latency_rating(results.latency_ms);
    let packet_loss_rating = self.get_packet_loss_rating(results.packet_loss_pct); // 新增
    
    println!("\n{}", "Connection Quality:".bright_white().bold());
    println!("  Download:  {}", download_rating);
    println!("  Latency:   {}", latency_rating);
    println!("  Packet Loss: {}", packet_loss_rating); // 新增
}

    fn get_speed_rating(&self, mbps: f64) -> String {
        if mbps >= 100.0 {
            "Excellent".bright_green().to_string()
        } else if mbps >= 50.0 {
            "Good".green().to_string()
        } else if mbps >= 25.0 {
            "Fair".yellow().to_string()
        } else if mbps >= 10.0 {
            "Poor".red().to_string()
        } else {
            "Very Poor".bright_red().to_string()
        }
    }

    fn get_latency_rating(&self, ms: f64) -> String {
        if ms <= 20.0 {
            "Excellent".bright_green().to_string()
        } else if ms <= 50.0 {
            "Good".green().to_string()
        } else if ms <= 100.0 {
            "Fair".yellow().to_string()
        } else if ms <= 200.0 {
            "Poor".red().to_string()
        } else {
            "Very Poor".bright_red().to_string()
        }
    }

    // 新增：丢包率评级方法
    fn get_packet_loss_rating(&self, packet_loss_pct: f64) -> String {
        if packet_loss_pct <= 0.5 {
            "Excellent".bright_green().to_string()
        } else if packet_loss_pct <= 1.0 {
            "Good".green().to_string()
        } else if packet_loss_pct <= 2.0 {
            "Fair".yellow().to_string()
        } else if packet_loss_pct <= 5.0 {
            "Poor".red().to_string()
        } else {
            "Very Poor".bright_red().to_string()
        }
    }
    async fn measure_packet_loss(&self) -> Result<f64> {
        println!("{}", "Measuring packet loss...".yellow());
        let pb = ProgressBar::new(self.config.packet_loss_tests as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"));

        let mut successful = 0;
        let mut failed = 0;
        
        for _ in 0..self.config.packet_loss_tests {
            let result = self.client
                .get(CLOUDFLARE_DOWNLOAD_URL)
                .query(&[("bytes", "1")])
                .send()
                .await;
            
            match result {
                Ok(response) if response.status().is_success() => {
                    successful += 1;
                }
                _ => {
                    failed += 1;
                }
            }
            
            pb.inc(1);
            sleep(Duration::from_millis(20)).await; // 更短的间隔以提高测试敏感度
        }
        
        pb.finish_with_message("Packet loss measurement complete");
        
        let packet_loss_pct = (failed as f64 / self.config.packet_loss_tests as f64) * 100.0;
        Ok(packet_loss_pct)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Command::new("speed-cloudflare-cli")
        .version("0.1.1")
        .about("A fast Rust implementation of Cloudflare speed test CLI")
        .author("Your Name")
        .arg(
            Arg::new("count")
                .short('c')
                .long("count")
                .value_name("COUNT")
                .help("Number of test runs")
                .default_value("1")
        )
        .arg(
            Arg::new("json")
                .short('j')
                .long("json")
                .help("Output results in JSON format")
                .action(clap::ArgAction::SetTrue)
        )
        .arg(
            Arg::new("ip_version")
                .short('i')
                .long("ip-version")
                .value_name("VERSION")
                .help("IP version to use (4 for IPv4, 6 for IPv6, auto for automatic)")
                .default_value("auto")
        )
        .get_matches();

    let _count: usize = matches.get_one::<String>("count")
        .unwrap()
        .parse()
        .unwrap_or(1);
    
    let json_output = matches.get_flag("json");
    
    // 解析IP版本参数
    let ip_version_str = matches.get_one::<String>("ip_version").unwrap();
    let ip_version = match ip_version_str.as_str() {
        "4" => IpVersion::V4,
        "6" => IpVersion::V6,
        "auto" => IpVersion::Auto,
        _ => {
            eprintln!("Invalid IP version. Use 4, 6, or auto.");
            std::process::exit(1);
        }
    };
    
    let config = SpeedTestConfig {
        ip_version,
        ..Default::default()
    };
    
    let speed_test = SpeedTest::new(config);
    
    match speed_test.run().await {
        Ok(results) => {
            if json_output {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                speed_test.display_results(&results);
            }
        }
        Err(e) => {
            eprintln!("{}: {}", "Error".bright_red(), e);
            std::process::exit(1);
        }
    }
    
    Ok(())
}
