use std::thread::sleep;
use std::time::Duration;
use log::Level;
use fast_log::config::Config;

fn main() {
    let log= fast_log::init(Config::new().console()).unwrap();
    log::debug!("Commencing yak shaving{}", 0);
    sleep(Duration::from_secs(1));
}