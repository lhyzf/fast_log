use std::cell::RefCell;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Write, Error, Seek, SeekFrom};

use chrono::Local;
use crossbeam_channel::{Receiver, Sender};
use zip::write::FileOptions;

use crate::appender::{FastLogRecord, LogAppender};
use crate::consts::LogSize;

/// split log file allow zip compress log
pub struct FileSplitAppender {
    cell: RefCell<FileSplitAppenderData>
}

pub struct ZipPack {
    pub data: Vec<u8>,
    pub log_file_name: String,
}

/// split log file allow zip compress log
pub struct FileSplitAppenderData {
    max_split_bytes: usize,
    dir_path: String,
    file: File,
    zip_compress: bool,
    sender: Sender<ZipPack>,
    //cache data
    temp_bytes: usize,
    temp_data: Option<Vec<u8>>,
}


impl FileSplitAppender {
    ///split_log_bytes: log file data bytes(MB) splite
    ///dir_path the dir
    pub fn new(dir_path: &str, max_temp_size: LogSize, allow_zip_compress: bool) -> FileSplitAppender {
        if !dir_path.is_empty() && dir_path.ends_with(".log") {
            panic!("FileCompactionAppender only support new from path,for example: 'logs/xx/'");
        }
        if !dir_path.is_empty() && !dir_path.ends_with("/") {
            panic!("FileCompactionAppender only support new from path,for example: 'logs/xx/'");
        }
        if !dir_path.is_empty() {
            DirBuilder::new().create(dir_path);
        }
        let first_file_path = format!("{}{}.log", dir_path, "temp");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(first_file_path.as_str());
        if file.is_err() {
            panic!("[fast_log] open and create file fail:{}", file.err().unwrap());
        }
        let mut file = file.unwrap();
        let mut temp_bytes = 0;
        match file.metadata() {
            Ok(m) => {
                temp_bytes = m.len() as usize;
            }
            _ => {}
        }
        let mut temp_data = vec![];
        file.read_to_end(&mut temp_data);
        file.seek(SeekFrom::Start(temp_bytes as u64));
        let (s, r) = crossbeam_channel::bounded(100);
        spawn_do_zip(r);
        Self {
            cell: RefCell::new(FileSplitAppenderData {
                max_split_bytes: max_temp_size.get_len(),
                temp_bytes: temp_bytes,
                temp_data: Some(temp_data),
                dir_path: dir_path.to_string(),
                file: file,
                zip_compress: allow_zip_compress,
                sender: s,
            })
        }
    }
}

impl LogAppender for FileSplitAppender {
    fn do_log(&self, record: &FastLogRecord) {
        let log_data = record.formated.as_str();
        let mut data = self.cell.borrow_mut();
        if data.temp_bytes >= data.max_split_bytes {
            if data.zip_compress {
                //to zip
                match data.temp_data.take() {
                    Some(temp) => {
                        data.sender.send(ZipPack {
                            data: temp,
                            log_file_name: format!("{}{}.log", data.dir_path, "temp"),
                        });
                    }
                    _ => {}
                }
            } else {
                let log_name = format!("{}{}{}.log", data.dir_path, "temp", format!("{:36}", Local::now())
                    .replace(":", "_")
                    .replace(" ", "_"));
                let lanme = log_name.as_str();
                let f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_name);
                match f {
                    Ok(mut f) => {
                        f.write_all(&data.temp_data.take().unwrap());
                        f.flush();
                    }
                    _ => {}
                }
            }
            //reset data
            data.file.set_len(0);
            data.file.seek(SeekFrom::Start(0));
            data.temp_bytes = 0;
            data.temp_data = Some(vec![]);
        }
        let write_bytes = data.file.write(log_data.as_bytes());
        data.file.flush();
        match write_bytes {
            Ok(size) => {
                let bytes = log_data.as_bytes();
                for x in bytes {
                    data.temp_data.as_mut().unwrap().push(*x);
                }
                data.temp_bytes += size;
            }
            _ => {}
        }
    }
}


fn spawn_do_zip(r: Receiver<ZipPack>) {
    std::thread::spawn(move || {
        loop {
            match r.recv() {
                Ok(pack) => {
                    do_zip(pack);
                }
                _ => {}
            }
        }
    });
}

/// write an ZipPack
pub fn do_zip(pack: ZipPack) {
    let log_file_path = pack.log_file_name.as_str();
    if log_file_path.is_empty() || pack.data.is_empty() {
        return;
    }
    let log_names: Vec<&str> = log_file_path.split("/").collect();
    let log_name = log_names[log_names.len() - 1];

    //make zip
    let zip_path = log_file_path.replace(".log", &format!("_{}.zip", Local::now().format("%Y_%m_%dT%H_%M_%S").to_string()));
    let zip_file = std::fs::File::create(&zip_path);
    if zip_file.is_err() {
        println!("[fast_log] create(&{}) fail:{}", zip_path, zip_file.err().unwrap());
        return;
    }
    let zip_file = zip_file.unwrap();

    //write zip bytes data
    let mut zip = zip::ZipWriter::new(zip_file);
    zip.start_file(log_name, FileOptions::default());
    zip.write_all(pack.data.as_slice());
    zip.flush();
    let finish = zip.finish();
    if finish.is_err() {
        println!("[fast_log] try zip fail{:?}", finish.err());
        return;
    }
}

#[cfg(test)]
mod test {
    use std::io::Write;

    use zip::write::FileOptions;

    #[test]
    fn test_zip() {
        let zip_file = std::fs::File::create("F:/rust_project/fast_log/target/logs/0.zip");
        match zip_file {
            Ok(zip_file) => {
                let mut zip = zip::ZipWriter::new(zip_file);
                zip.start_file("0.log", FileOptions::default());
                zip.write("sadfsadfsadf".as_bytes());
                let finish = zip.finish();
                match finish {
                    Ok(f) => {
                        //std::fs::remove_file("F:/rust_project/fast_log/target/logs/0.log");
                    }
                    Err(e) => {
                        //nothing
                        panic!(e)
                    }
                }
            }
            Err(e) => {
                panic!(e)
            }
        }
    }
}