use std::borrow::Borrow;
use std::sync::atomic::AtomicI32;
use log::{Level, Metadata, Record};
use parking_lot::RwLock;

use crate::appender::{Command, FastLogFormatRecord, FastLogRecord, LogAppender, RecordFormat};
use crate::consts::LogSize;
use crate::error::LogError;
use crate::filter::{Filter, NoFilter};
use crate::plugin::console::ConsoleAppender;
use crate::plugin::file::FileAppender;
use crate::plugin::file_split::{FileSplitAppender, RollingType, Packer};
use crate::wait::FastLogWaitGroup;
use std::result::Result::Ok;
use std::time::{SystemTime, Duration};
use std::sync::Arc;
use std::sync::mpsc::SendError;

lazy_static! {
    static ref LOG_SENDER: RwLock<Option<LoggerSender>> = RwLock::new(Option::None);
}

pub struct LoggerSender {
    pub filter: Box<dyn Filter>,
    pub inner: crossbeam::channel::Sender<FastLogRecord>,
}

impl LoggerSender {
    pub fn new(filter: Box<dyn Filter>) -> (Self, crossbeam::channel::Receiver<FastLogRecord>) {
        let (s, r) = crossbeam::channel::unbounded();
        (Self { inner: s, filter }, r)
    }
    pub fn send(&self, data: FastLogRecord) -> Result<(), crossbeam::channel::SendError<FastLogRecord>> {
        self.inner.send(data)
    }
}

fn set_log(level: log::Level, filter: Box<dyn Filter>) -> crossbeam::channel::Receiver<FastLogRecord> {
    LOGGER.set_level(level);
    let mut w = LOG_SENDER.write();
    let (log, recv) = LoggerSender::new(filter);
    *w = Some(log);
    return recv;
}

pub struct Logger {
    level: AtomicI32,
}

impl Logger {
    pub fn set_level(&self, level: log::Level) {
        self.level
            .swap(level as i32, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn get_level(&self) -> log::Level {
        match self.level.load(std::sync::atomic::Ordering::Relaxed) {
            1 => Level::Error,
            2 => Level::Warn,
            3 => Level::Info,
            4 => Level::Debug,
            5 => Level::Trace,
            _ => panic!("error log level!"),
        }
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.get_level()
    }
    fn log(&self, record: &Record) {
        //send
        if let Some(sender) = LOG_SENDER.read().as_ref() {
            if !sender.filter.filter(record) {
                if let Some(v) = record.module_path() {
                    if v == "cogo::io::sys::select" {
                        return;
                    }
                }
                let fast_log_record = FastLogRecord {
                    command: Command::CommandRecord,
                    level: record.level(),
                    target: record.metadata().target().to_string(),
                    args: record.args().to_string(),
                    module_path: record.module_path().unwrap_or_default().to_string(),
                    file: record.file().unwrap_or_default().to_string(),
                    line: record.line().clone(),
                    now: SystemTime::now(),
                    formated: String::new(),
                };
                sender.send(fast_log_record);
            }
        }
    }
    fn flush(&self) {}
}

static LOGGER: Logger = Logger {
    level: AtomicI32::new(1),
};

/// initializes the log file path
/// log_file_path:  example->  "test.log"
/// channel_cup: example -> 1000
pub fn init_log(
    log_file_path: &str,
    level: log::Level,
    mut filter: Option<Box<dyn Filter>>,
    debug_mode: bool,
) -> Result<FastLogWaitGroup, LogError> {
    let mut appenders: Vec<Box<dyn LogAppender>> = vec![Box::new(FileAppender::new(log_file_path))];
    if debug_mode {
        appenders.push(Box::new(ConsoleAppender {}));
    }
    let mut log_filter: Box<dyn Filter> = Box::new(NoFilter {});
    if filter.is_some() {
        log_filter = filter.take().unwrap();
    }
    return init_custom_log(
        appenders,
        level,
        log_filter,
        Box::new(FastLogFormatRecord::new()),
    );
}

/// initializes the log file path
/// log_dir_path:  example->  "log/"
/// max_temp_size: do zip if temp log full
/// allow_zip_compress: zip compress log file
/// filter: log filter
/// packer: you can use ZipPacker or LZ4Packer or custom your Packer
/// temp is "temp.log"
pub fn init_split_log(
    log_dir_path: &str,
    max_temp_size: LogSize,
    rolling_type: RollingType,
    level: log::Level,
    mut filter: Option<Box<dyn Filter>>,
    packer: Box<dyn Packer>,
    allow_console_log: bool,
) -> Result<FastLogWaitGroup, LogError> {
    let mut appenders: Vec<Box<dyn LogAppender>> = vec![Box::new(FileSplitAppender::new(
        log_dir_path,
        max_temp_size,
        rolling_type,
        packer,
    ))];
    if allow_console_log {
        appenders.push(Box::new(ConsoleAppender {}));
    }
    let mut log_filter: Box<dyn Filter> = Box::new(NoFilter {});
    if filter.is_some() {
        log_filter = filter.take().unwrap();
    }
    return init_custom_log(
        appenders,
        level,
        log_filter,
        Box::new(FastLogFormatRecord::new()),
    );
}

pub fn init_custom_log(
    appenders: Vec<Box<dyn LogAppender>>,
    level: log::Level,
    filter: Box<dyn Filter>,
    format: Box<dyn RecordFormat>,
) -> Result<FastLogWaitGroup, LogError> {
    if appenders.is_empty() {
        return Err(LogError::from("[fast_log] appenders can not be empty!"));
    }
    let wait_group = FastLogWaitGroup::new();
    let main_recv = set_log(level, filter);
    //main recv data
    let wait_group_back = wait_group.clone();
    std::thread::spawn(move || {
        let mut recever_vec = vec![];
        let mut sender_vec: Vec<cogo::std::sync::mpsc::Sender<Arc<FastLogRecord>>> = vec![];
        for a in appenders {
            let (s, r) = cogo::std::sync::mpsc::channel();
            sender_vec.push(s);
            recever_vec.push((r, a));
        }
        for (recever, appender) in recever_vec {
            let current_wait_group = wait_group_back.clone();
            if appender.type_name().starts_with("fast_log::plugin") {
                // if is file appender, use thread spawn
                std::thread::spawn(move || {
                    loop {
                        if let Ok(msg) = recever.recv() {
                            if msg.command.eq(&Command::CommandExit) {
                                drop(current_wait_group);
                                break;
                            }
                            appender.do_log(msg.as_ref());
                        } else {
                            cogo::coroutine::yield_now();
                        }
                    }
                });
            } else {
                // if is network appender, use thread spawn
                cogo::go!(cogo::coroutine::Builder::new().stack_size(2*0x1000),move ||{
                loop{
                    if let Ok(msg) = recever.recv(){
                     if msg.command.eq(&Command::CommandExit) {
                        drop(current_wait_group);
                        break;
                     }
                     appender.do_log(msg.as_ref());
                    }
                }
            });
            }
        }
        loop {
            //recv
            let data = main_recv.recv();
            if let Ok(mut data) = data {
                data.formated = format.do_format(&mut data);
                let data = Arc::new(data);
                for x in &sender_vec {
                    x.send(data.clone());
                }
                if data.command.eq(&Command::CommandExit) {
                    drop(wait_group_back);
                    break;
                }
            } else {
                cogo::coroutine::yield_now();
            }
        }
    });
    let r = log::set_logger(&LOGGER).map(|()| log::set_max_level(level.to_level_filter()));
    if r.is_err() {
        return Err(LogError::from(r.err().unwrap()));
    } else {
        return Ok(wait_group);
    }
}

pub fn exit() -> Result<(), LogError> {
    let sender = LOG_SENDER.read();
    if sender.is_some() {
        let sender = sender.as_ref().unwrap();
        let fast_log_record = FastLogRecord {
            command: Command::CommandExit,
            level: log::Level::Info,
            target: String::new(),
            args: String::new(),
            module_path: String::new(),
            file: String::new(),
            line: None,
            now: SystemTime::now(),
            formated: String::new(),
        };
        let result = sender.send(fast_log_record);
        match result {
            Ok(()) => {
                return Ok(());
            }
            _ => {}
        }
    }

    return Err(LogError::E("[fast_log] exit fail!".to_string()));
}


pub fn flush() -> Result<(), LogError> {
    let sender = LOG_SENDER.read();
    if sender.is_some() {
        let sender = sender.as_ref().unwrap();
        let fast_log_record = FastLogRecord {
            command: Command::CommandFlush,
            level: log::Level::Info,
            target: String::new(),
            args: String::new(),
            module_path: String::new(),
            file: String::new(),
            line: None,
            now: SystemTime::now(),
            formated: String::new(),
        };
        let result = sender.send(fast_log_record);
        match result {
            Ok(()) => {
                return Ok(());
            }
            _ => {}
        }
    }
    return Err(LogError::E("[fast_log] flush fail!".to_string()));
}
