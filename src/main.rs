extern crate log;
extern crate simplelog;

use std::sync::{Arc, Mutex};

mod filter;
mod input;
mod output;
mod utils;

use simplelog::*;
use tokio::runtime::Builder;

use crate::output::{player, write_hls};
use crate::utils::{init_config, init_logging, validate_ffmpeg, GlobalConfig};

fn main() {
    init_config();
    let config = GlobalConfig::global();

    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();
    let rt_handle = runtime.handle();
    let is_terminated: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    let logging = init_logging(rt_handle.clone(), is_terminated.clone());
    CombinedLogger::init(logging).unwrap();

    validate_ffmpeg();

    if config.out.mode.to_lowercase() == "hls".to_string() {
        write_hls(rt_handle, is_terminated);
    } else {
        player(rt_handle, is_terminated);
    }
}
