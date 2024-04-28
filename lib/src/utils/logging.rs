extern crate log;
extern crate simplelog;

use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc, Mutex},
    thread::{self, sleep},
    time::Duration,
};

use chrono::prelude::*;
use file_rotate::{
    compression::Compression,
    suffix::{AppendTimestamp, DateFrom, FileLimit},
    ContentLimit, FileRotate, TimeFrequency,
};
use lettre::{
    message::header, transport::smtp::authentication::Credentials, Message, SmtpTransport,
    Transport,
};
use log::{Level, LevelFilter, Log, Metadata, Record};
use regex::Regex;
use simplelog::*;

use crate::utils::{PlayoutConfig, ProcessControl};

/// send log messages to mail recipient
pub fn send_mail(cfg: &PlayoutConfig, msg: String) {
    let recipient = cfg
        .mail
        .recipient
        .split_terminator([',', ';', ' '])
        .filter(|s| s.contains('@'))
        .map(|s| s.trim())
        .collect::<Vec<&str>>();

    let mut message = Message::builder()
        .from(cfg.mail.sender_addr.parse().unwrap())
        .subject(&cfg.mail.subject)
        .header(header::ContentType::TEXT_PLAIN);

    for r in recipient {
        message = message.to(r.parse().unwrap());
    }

    if let Ok(mail) = message.body(clean_string(&msg)) {
        let credentials =
            Credentials::new(cfg.mail.sender_addr.clone(), cfg.mail.sender_pass.clone());

        let mut transporter = SmtpTransport::relay(cfg.mail.smtp_server.clone().as_str());

        if cfg.mail.starttls {
            transporter = SmtpTransport::starttls_relay(cfg.mail.smtp_server.clone().as_str());
        }

        let mailer = transporter.unwrap().credentials(credentials).build();

        // Send the mail
        if let Err(e) = mailer.send(&mail) {
            error!("Could not send mail: {e}");
        }
    } else {
        error!("Mail Message failed!");
    }
}

/// Basic Mail Queue
///
/// Check every give seconds for messages and send them.
fn mail_queue(
    cfg: PlayoutConfig,
    proc_ctl: ProcessControl,
    messages: Arc<Mutex<Vec<String>>>,
    interval: u64,
) {
    while !proc_ctl.is_terminated.load(Ordering::SeqCst) {
        let mut msg = messages.lock().unwrap();

        if msg.len() > 0 {
            send_mail(&cfg, msg.join("\n"));

            msg.clear();
        }

        drop(msg);

        sleep(Duration::from_secs(interval));
    }
}

/// Self made Mail Log struct, to extend simplelog.
pub struct LogMailer {
    level: LevelFilter,
    pub config: Config,
    messages: Arc<Mutex<Vec<String>>>,
    last_messages: Arc<Mutex<Vec<String>>>,
}

impl LogMailer {
    pub fn new(
        log_level: LevelFilter,
        config: Config,
        messages: Arc<Mutex<Vec<String>>>,
    ) -> Box<LogMailer> {
        Box::new(LogMailer {
            level: log_level,
            config,
            messages,
            last_messages: Arc::new(Mutex::new(vec![String::new()])),
        })
    }
}

impl Log for LogMailer {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let rec = record.args().to_string();
            let mut last_msgs = self.last_messages.lock().unwrap();

            // put message only to mail queue when it differs from last message
            // this we do to prevent spamming the mail box
            // also ignore errors from lettre mail module, because it prevents program from closing
            if !last_msgs.contains(&rec) && !rec.contains("lettre") {
                if last_msgs.len() > 2 {
                    last_msgs.clear()
                }
                last_msgs.push(rec.clone());
                let local: DateTime<Local> = Local::now();
                let time_stamp = local.format("[%Y-%m-%d %H:%M:%S%.3f]");
                let level = record.level().to_string().to_uppercase();
                let full_line = format!("{time_stamp} [{level: >5}] {rec}");

                self.messages.lock().unwrap().push(full_line);
            }
        }
    }

    fn flush(&self) {}
}

impl SharedLogger for LogMailer {
    fn level(&self) -> LevelFilter {
        self.level
    }

    fn config(&self) -> Option<&Config> {
        Some(&self.config)
    }

    fn as_log(self: Box<Self>) -> Box<dyn Log> {
        Box::new(*self)
    }
}

/// Workaround to remove color information from log
fn clean_string(text: &str) -> String {
    let regex = Regex::new(r"\x1b\[[0-9;]*[mGKF]").unwrap();

    regex.replace_all(text, "").to_string()
}

/// Initialize our logging, to have:
///
/// - console logger
/// - file logger
/// - mail logger
pub fn init_logging(
    config: &PlayoutConfig,
    proc_ctl: Option<ProcessControl>,
    messages: Option<Arc<Mutex<Vec<String>>>>,
) -> Vec<Box<dyn SharedLogger>> {
    let config_clone = config.clone();
    let app_config = config.logging.clone();
    let mut time_level = LevelFilter::Off;
    let mut app_logger: Vec<Box<dyn SharedLogger>> = vec![];

    if app_config.timestamp {
        time_level = LevelFilter::Error;
    }

    let mut log_config = ConfigBuilder::new()
        .set_thread_level(LevelFilter::Off)
        .set_target_level(LevelFilter::Off)
        .add_filter_ignore_str("hyper")
        .add_filter_ignore_str("libc")
        .add_filter_ignore_str("neli")
        .add_filter_ignore_str("reqwest")
        .add_filter_ignore_str("rpc")
        .add_filter_ignore_str("rustls")
        .add_filter_ignore_str("serial_test")
        .add_filter_ignore_str("sqlx")
        .add_filter_ignore_str("tiny_http")
        .set_level_padding(LevelPadding::Left)
        .set_time_level(time_level)
        .clone();

    if app_config.local_time {
        log_config = match log_config.set_time_offset_to_local() {
            Ok(local) => local.clone(),
            Err(_) => log_config,
        };
    };

    if app_config.log_to_file && app_config.path.exists() {
        let file_config = log_config
            .clone()
            .set_time_format_custom(format_description!(
                "[[[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:5]]"
            ))
            .build();
        let mut log_path = PathBuf::from("logs/ffplayout.log");

        if app_config.path.is_dir() {
            log_path = app_config.path.join("ffplayout.log");
        } else if app_config.path.is_file() {
            log_path = app_config.path
        } else {
            eprintln!("Logging path not exists!")
        }

        let log_file = FileRotate::new(
            log_path,
            AppendTimestamp::with_format(
                "%Y-%m-%d",
                FileLimit::MaxFiles(app_config.backup_count),
                DateFrom::DateYesterday,
            ),
            ContentLimit::Time(TimeFrequency::Daily),
            Compression::None,
            #[cfg(unix)]
            None,
        );

        app_logger.push(WriteLogger::new(app_config.level, file_config, log_file));
    } else {
        let term_config = log_config
            .clone()
            .set_level_color(Level::Trace, Some(Color::Ansi256(11)))
            .set_level_color(Level::Debug, Some(Color::Ansi256(12)))
            .set_level_color(Level::Info, Some(Color::Ansi256(10)))
            .set_level_color(Level::Warn, Some(Color::Ansi256(208)))
            .set_level_color(Level::Error, Some(Color::Ansi256(9)))
            .set_time_format_custom(format_description!(
                "\x1b[[30;1m[[[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:5]]\x1b[[0m"
            ))
            .build();

        app_logger.push(TermLogger::new(
            app_config.level,
            term_config,
            TerminalMode::Mixed,
            ColorChoice::Auto,
        ));
    }

    // set mail logger only the recipient is set in config
    if config.mail.recipient.contains('@') && config.mail.recipient.contains('.') {
        let messages_clone = messages.clone().unwrap();
        let interval = config.mail.interval;

        thread::spawn(move || {
            mail_queue(config_clone, proc_ctl.unwrap(), messages_clone, interval)
        });

        let mail_config = log_config.build();

        let filter = match config.mail.mail_level.to_lowercase().as_str() {
            "info" => LevelFilter::Info,
            "warning" => LevelFilter::Warn,
            _ => LevelFilter::Error,
        };

        app_logger.push(LogMailer::new(filter, mail_config, messages.unwrap()));
    }

    app_logger
}
