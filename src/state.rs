use super::config::*;
use super::errors::MyError;
use super::functions::*;
use super::nums::*;
use super::session::*;
use chrono::prelude::*;
use log::error;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use termion::{clear, color, cursor, style};

pub const STARTING_POINT: u16 = 3;
pub const DOWN_ARROW: char = '\u{21D3}';
pub const RIGHT_ARROW: char = '\u{21D2}';
pub const FX_CONFIG_DIR: &str = "felix";
pub const CONFIG_FILE: &str = "config.toml";
pub const TRASH: &str = "trash";
pub const WHEN_EMPTY: &str = "Are you sure to empty the trash directory? (if yes: y)";

macro_rules! print_item {
    ($color: expr, $name: expr, $time: expr, $selected: expr, $layout: expr) => {
        if *($selected) {
            print!(
                "{}{}{}{}{}{} {}{}{}",
                $color,
                style::Invert,
                $name,
                style::Reset,
                cursor::Left(100),
                cursor::Right($layout.time_start_pos - 1),
                style::Invert,
                $time,
                style::Reset
            );
        } else {
            print!(
                "{}{}{}{} {}{}",
                $color,
                $name,
                cursor::Left(100),
                cursor::Right($layout.time_start_pos - 1),
                $time,
                color::Fg(color::Reset)
            );
        }
        if $layout.terminal_column > $layout.time_start_pos + TIME_WIDTH {
            print!("{}", clear::AfterCursor);
        }
    };
}
#[derive(Clone)]
pub struct State {
    pub list: Vec<ItemInfo>,
    pub registered: Vec<ItemInfo>,
    pub manipulations: Manipulation,
    pub current_dir: PathBuf,
    pub trash_dir: PathBuf,
    pub default: String,
    pub commands: HashMap<String, String>,
    pub sort_by: SortKey,
    pub layout: Layout,
    pub show_hidden: bool,
    pub rust_log: Option<String>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct ItemInfo {
    pub file_type: FileType,
    pub file_name: String,
    pub file_path: std::path::PathBuf,
    pub symlink_dir_path: Option<PathBuf>,
    pub file_size: u64,
    pub file_ext: Option<OsString>,
    pub modified: Option<String>,
    pub is_hidden: bool,
    pub selected: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileType {
    Directory,
    File,
    Symlink,
}

#[derive(Clone)]
pub struct Layout {
    pub y: u16,
    pub terminal_row: u16,
    pub terminal_column: u16,
    pub name_max_len: usize,
    pub time_start_pos: u16,
    pub use_full: Option<bool>,
    pub option_name_len: Option<usize>,
    pub colors: Color,
}

#[derive(Debug, Clone)]
pub struct Manipulation {
    pub count: usize,
    pub manip_list: Vec<ManipKind>,
}

#[derive(Debug, Clone)]
pub enum ManipKind {
    Delete(DeletedFiles),
    Put(PutFiles),
    Rename(RenamedFile),
}

#[derive(Debug, Clone)]
pub struct RenamedFile {
    pub original_name: PathBuf,
    pub new_name: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PutFiles {
    pub original: Vec<ItemInfo>,
    pub put: Vec<PathBuf>,
    pub dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DeletedFiles {
    pub trash: Vec<PathBuf>,
    pub original: Vec<ItemInfo>,
    pub dir: PathBuf,
}

impl Default for State {
    fn default() -> Self {
        let config = read_config().unwrap_or_else(|_| panic!("Something wrong with config file."));
        let session =
            read_session().unwrap_or_else(|_| panic!("Something wrong with session file."));
        let (column, row) =
            termion::terminal_size().unwrap_or_else(|_| panic!("Cannot detect terminal size."));
        if column < 21 {
            error!("Too small terminal size.");
            panic!("Panic due to terminal size (less than 21 columns).")
        };
        if row < 4 {
            error!("Too small terminal size.");
            panic!("Panic due to terminal size (less than 4 rows).")
        };
        let (time_start, name_max) =
            make_layout(column, config.use_full_width, config.item_name_length);

        State {
            list: Vec::new(),
            registered: Vec::new(),
            manipulations: Manipulation {
                count: 0,
                manip_list: Vec::new(),
            },
            current_dir: PathBuf::new(),
            trash_dir: PathBuf::new(),
            default: config.default,
            commands: to_extension_map(&config.exec),
            sort_by: session.sort_by,
            layout: Layout {
                y: STARTING_POINT,
                terminal_row: row,
                terminal_column: column,
                name_max_len: name_max,
                time_start_pos: time_start,
                use_full: config.use_full_width,
                option_name_len: config.item_name_length,
                colors: Color {
                    dir_fg: config.color.dir_fg,
                    file_fg: config.color.file_fg,
                    symlink_fg: config.color.symlink_fg,
                },
            },
            show_hidden: session.show_hidden,
            rust_log: std::env::var("RUST_LOG").ok(),
        }
    }
}

impl State {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn refresh(&mut self, column: u16, row: u16, nums: &Num, cursor_pos: u16) {
        let (time_start, name_max) =
            make_layout(column, self.layout.use_full, self.layout.option_name_len);

        self.layout.terminal_row = row;
        self.layout.terminal_column = column;
        self.layout.name_max_len = name_max;
        self.layout.time_start_pos = time_start;

        clear_and_show(&self.current_dir);
        self.list_up(nums.skip);
        self.move_cursor(nums, cursor_pos);
    }

    pub fn get_item(&self, index: usize) -> Result<&ItemInfo, MyError> {
        self.list.get(index).ok_or_else(|| {
            MyError::IoError(std::io::Error::new(
                ErrorKind::NotFound,
                "Cannot choose item.",
            ))
        })
    }

    pub fn open_file(&self, index: usize) -> Result<ExitStatus, MyError> {
        let item = self.get_item(index)?;
        let path = &item.file_path;
        let map = &self.commands;
        let extention = path.extension();

        match extention {
            Some(extention) => {
                let ext = extention.to_ascii_lowercase().into_string().unwrap();
                match map.get(&ext) {
                    Some(command) => {
                        let mut ex = Command::new(command);
                        ex.arg(path).status().map_err(MyError::IoError)
                    }
                    None => {
                        let mut ex = Command::new(&self.default);
                        ex.arg(path).status().map_err(MyError::IoError)
                    }
                }
            }

            None => {
                let mut ex = Command::new(&self.default);
                ex.arg(path).status().map_err(MyError::IoError)
            }
        }
    }

    //Discard undone manipulations when new manipulation is pushed.
    pub fn branch_manip(&mut self) {
        if self.manipulations.count == 0 {
            return;
        }
        for _i in 0..self.manipulations.count {
            self.manipulations.manip_list.pop();
        }
    }

    pub fn remove_and_yank(
        &mut self,
        targets: &[ItemInfo],
        new_manip: bool,
    ) -> Result<(), MyError> {
        self.registered.clear();
        let total_selected = targets.len();
        let mut trash_vec = Vec::new();
        for (i, item) in targets.iter().enumerate() {
            let item = item.clone();
            print!(
                " {}{}{}",
                cursor::Goto(2, 2),
                clear::CurrentLine,
                display_count(i, total_selected)
            );
            match item.file_type {
                FileType::Directory => match self.remove_and_yank_dir(item.clone(), new_manip) {
                    Err(e) => {
                        return Err(e);
                    }
                    Ok(path) => trash_vec.push(path),
                },
                FileType::File | FileType::Symlink => {
                    match self.remove_and_yank_file(item.clone(), new_manip) {
                        Err(e) => {
                            return Err(e);
                        }
                        Ok(path) => trash_vec.push(path),
                    }
                }
            }
        }
        if new_manip {
            self.branch_manip();
            //push deleted item information to manipulations
            self.manipulations
                .manip_list
                .push(ManipKind::Delete(DeletedFiles {
                    trash: trash_vec,
                    original: targets.to_vec(),
                    dir: self.current_dir.clone(),
                }));
            self.manipulations.count = 0;
        }

        Ok(())
    }

    pub fn remove_and_yank_file(
        &mut self,
        item: ItemInfo,
        new_manip: bool,
    ) -> Result<PathBuf, MyError> {
        //prepare from and to for copy
        let from = &item.file_path;
        let mut to = PathBuf::new();

        if item.file_type == FileType::Symlink && !from.exists() {
            match Command::new("rm").arg(from).status() {
                Ok(_) => Ok(PathBuf::new()),
                Err(e) => Err(MyError::IoError(e)),
            }
        } else {
            let name = &item.file_name;
            let mut rename = Local::now().timestamp().to_string();
            rename.push('_');
            rename.push_str(name);

            if new_manip {
                to = self.trash_dir.join(&rename);

                //copy
                if std::fs::copy(from, &to).is_err() {
                    return Err(MyError::FileCopyError {
                        msg: format!("Cannot copy item: {:?}", from),
                    });
                }

                self.push_to_registered(&item, to.clone(), rename);
            }

            //remove original
            if std::fs::remove_file(from).is_err() {
                return Err(MyError::FileRemoveError {
                    msg: format!("Cannot Remove item: {:?}", from),
                });
            }

            Ok(to)
        }
    }

    pub fn remove_and_yank_dir(
        &mut self,
        item: ItemInfo,
        new_manip: bool,
    ) -> Result<PathBuf, MyError> {
        let mut trash_name = String::new();
        let mut base: usize = 0;
        let mut trash_path: std::path::PathBuf = PathBuf::new();
        let mut target: PathBuf;

        if new_manip {
            let len = walkdir::WalkDir::new(&item.file_path).into_iter().count();
            let unit = len / 5;
            for (i, entry) in walkdir::WalkDir::new(&item.file_path)
                .into_iter()
                .enumerate()
            {
                if i > unit * 4 {
                    print_process("[»»»»-]");
                } else if i > unit * 3 {
                    print_process("[»»»--]");
                } else if i > unit * 2 {
                    print_process("[»»---]");
                } else if i > unit {
                    print_process("[»----]");
                } else if i == 0 {
                    print_process(" [-----]");
                }
                let entry = entry?;
                let entry_path = entry.path();
                if i == 0 {
                    base = entry_path.iter().count();

                    trash_name = chrono::Local::now().timestamp().to_string();
                    trash_name.push('_');
                    let file_name = entry.file_name().to_str();
                    if file_name == None {
                        return Err(MyError::UTF8Error {
                            msg: "Cannot convert filename to UTF-8.".to_string(),
                        });
                    }
                    trash_name.push_str(file_name.unwrap());
                    trash_path = self.trash_dir.join(&trash_name);
                    std::fs::create_dir(&self.trash_dir.join(&trash_path))?;

                    continue;
                } else {
                    target = entry_path.iter().skip(base).collect();
                    target = trash_path.join(target);
                    if entry.file_type().is_dir() {
                        std::fs::create_dir_all(&target)?;
                        continue;
                    }

                    if let Some(parent) = entry_path.parent() {
                        if !parent.exists() {
                            std::fs::create_dir(parent)?;
                        }
                    }

                    if std::fs::copy(entry_path, &target).is_err() {
                        return Err(MyError::FileCopyError {
                            msg: format!("Cannot copy item: {:?}", entry_path),
                        });
                    }
                }
            }

            self.push_to_registered(&item, trash_path.clone(), trash_name);
        }

        //remove original
        if std::fs::remove_dir_all(&item.file_path).is_err() {
            return Err(MyError::FileRemoveError {
                msg: format!("Cannot Remove directory: {:?}", item.file_name),
            });
        }

        Ok(trash_path)
    }

    fn push_to_registered(&mut self, item: &ItemInfo, file_path: PathBuf, file_name: String) {
        let mut buf = item.clone();
        buf.file_path = file_path;
        buf.file_name = file_name;
        buf.selected = false;
        self.registered.push(buf);
    }

    pub fn yank_item(&mut self, index: usize, selected: bool) {
        self.registered.clear();
        if selected {
            for item in self.list.iter_mut().filter(|item| item.selected) {
                self.registered.push(item.clone());
            }
        } else {
            let item = self.get_item(index).unwrap().clone();
            self.registered.push(item);
        }
    }

    pub fn put_items(
        &mut self,
        targets: &[ItemInfo],
        target_dir: Option<PathBuf>,
    ) -> Result<(), MyError> {
        //make HashSet<String> of file_name
        let mut name_set = HashSet::new();
        let target_dir_clone = target_dir.clone();
        match target_dir_clone {
            None => {
                for item in self.list.iter() {
                    name_set.insert(item.file_name.clone());
                }
            }
            Some(path) => {
                for item in push_items(&path, &SortKey::Name, true)? {
                    name_set.insert(item.file_name);
                }
            }
        }

        //prepare for manipulations.push
        let mut put_v = Vec::new();

        let total_selected = targets.len();
        for (i, item) in targets.iter().enumerate() {
            print!(
                " {}{}{}",
                cursor::Goto(2, 2),
                clear::CurrentLine,
                display_count(i, total_selected)
            );
            match item.file_type {
                FileType::Directory => {
                    if let Ok(p) = self.put_dir(item, target_dir.clone(), &mut name_set) {
                        put_v.push(p);
                    }
                }
                FileType::File | FileType::Symlink => {
                    if let Ok(q) = self.put_file(item, target_dir.clone(), &mut name_set) {
                        put_v.push(q);
                    }
                }
            }
        }
        if target_dir.is_none() {
            self.branch_manip();
            //push put item information to manipulations
            self.manipulations.manip_list.push(ManipKind::Put(PutFiles {
                original: targets.to_owned(),
                put: put_v,
                dir: self.current_dir.clone(),
            }));
            self.manipulations.count = 0;
        }

        Ok(())
    }

    fn put_file(
        &mut self,
        item: &ItemInfo,
        target_dir: Option<PathBuf>,
        name_set: &mut HashSet<String>,
    ) -> Result<PathBuf, MyError> {
        match target_dir {
            None => {
                if item.file_path.parent() == Some(&self.trash_dir) {
                    let mut item = item.clone();
                    let rename = item.file_name.chars().skip(11).collect();
                    item.file_name = rename;
                    let rename = rename_file(&item, name_set);
                    let to = &self.current_dir.join(&rename);
                    if std::fs::copy(&item.file_path, to).is_err() {
                        return Err(MyError::FileCopyError {
                            msg: format!("Cannot copy item: {:?}", &item.file_path),
                        });
                    }
                    name_set.insert(rename);
                    Ok(to.to_path_buf())
                } else {
                    let rename = rename_file(item, name_set);
                    let to = &self.current_dir.join(&rename);
                    if std::fs::copy(&item.file_path, to).is_err() {
                        return Err(MyError::FileCopyError {
                            msg: format!("Cannot copy item: {:?}", &item.file_path),
                        });
                    }
                    name_set.insert(rename);
                    Ok(to.to_path_buf())
                }
            }
            Some(path) => {
                if item.file_path.parent() == Some(&self.trash_dir) {
                    let mut item = item.clone();
                    let rename = item.file_name.chars().skip(11).collect();
                    item.file_name = rename;
                    let rename = rename_file(&item, name_set);
                    let to = path.join(&rename);
                    if std::fs::copy(&item.file_path, to.clone()).is_err() {
                        return Err(MyError::FileCopyError {
                            msg: format!("Cannot copy item: {:?}", &item.file_path),
                        });
                    }
                    name_set.insert(rename);
                    Ok(to)
                } else {
                    let rename = rename_file(item, name_set);
                    let to = &path.join(&rename);
                    if std::fs::copy(&item.file_path, to).is_err() {
                        return Err(MyError::FileCopyError {
                            msg: format!("Cannot copy item: {:?}", &item.file_path),
                        });
                    }
                    name_set.insert(rename);
                    Ok(to.to_path_buf())
                }
            }
        }
    }

    fn put_dir(
        &mut self,
        buf: &ItemInfo,
        target_dir: Option<PathBuf>,
        name_set: &mut HashSet<String>,
    ) -> Result<PathBuf, MyError> {
        let mut base: usize = 0;
        let mut target: PathBuf = PathBuf::new();
        let original_path = &(buf).file_path;

        let len = walkdir::WalkDir::new(&original_path).into_iter().count();
        let unit = len / 5;
        for (i, entry) in walkdir::WalkDir::new(&original_path)
            .into_iter()
            .enumerate()
        {
            if i > unit * 4 {
                print_process("[»»»»-]");
            } else if i > unit * 3 {
                print_process("[»»»--]");
            } else if i > unit * 2 {
                print_process("[»»---]");
            } else if i > unit {
                print_process("[»----]");
            } else if i == 0 {
                print_process(" [»----]");
            }
            let entry = entry?;
            let entry_path = entry.path();
            if i == 0 {
                base = entry_path.iter().count();

                let parent = &original_path.parent().unwrap();
                if parent == &self.trash_dir {
                    let mut buf = buf.clone();
                    let rename: String = buf.file_name.chars().skip(11).collect();
                    buf.file_name = rename.clone();
                    target = match &target_dir {
                        None => self.current_dir.join(&rename),
                        Some(path) => path.join(&rename),
                    };
                    let rename = rename_dir(&buf, name_set);
                    name_set.insert(rename);
                } else {
                    let rename = rename_dir(buf, name_set);
                    target = match &target_dir {
                        None => self.current_dir.join(&rename),
                        Some(path) => path.join(&rename),
                    };
                    name_set.insert(rename);
                }
                std::fs::create_dir(&target)?;
                continue;
            } else {
                let child: PathBuf = entry_path.iter().skip(base).collect();
                let child = target.join(child);

                if entry.file_type().is_dir() {
                    std::fs::create_dir_all(child)?;
                    continue;
                } else if let Some(parent) = entry_path.parent() {
                    if !parent.exists() {
                        std::fs::create_dir(parent)?;
                    }
                }

                if std::fs::copy(entry_path, &child).is_err() {
                    return Err(MyError::FileCopyError {
                        msg: format!("Cannot copy item: {:?}", entry_path),
                    });
                }
            }
        }
        Ok(target)
    }

    pub fn print(&self, index: usize) {
        let item = &self.get_item(index).unwrap();
        let chars: Vec<char> = item.file_name.chars().collect();
        let name = if chars.len() > self.layout.name_max_len {
            let mut result = chars
                .iter()
                .take(self.layout.name_max_len - 2)
                .collect::<String>();
            result.push_str("..");
            result
        } else {
            item.file_name.clone()
        };
        let time = format_time(&item.modified);
        let selected = &item.selected;
        let color = match item.file_type {
            FileType::Directory => &self.layout.colors.dir_fg,
            FileType::File => &self.layout.colors.file_fg,
            FileType::Symlink => &self.layout.colors.symlink_fg,
        };
        match color {
            Colorname::AnsiValue(n) => {
                print_item!(
                    color::Fg(color::AnsiValue(*n)),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::Black => {
                print_item!(color::Fg(color::Black), name, time, selected, self.layout);
            }
            Colorname::Blue => {
                print_item!(color::Fg(color::Blue), name, time, selected, self.layout);
            }
            Colorname::Cyan => {
                print_item!(color::Fg(color::Cyan), name, time, selected, self.layout);
            }
            Colorname::Green => {
                print_item!(color::Fg(color::Green), name, time, selected, self.layout);
            }
            Colorname::LightBlack => {
                print_item!(
                    color::Fg(color::LightBlack),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::LightBlue => {
                print_item!(
                    color::Fg(color::LightBlue),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::LightCyan => {
                print_item!(
                    color::Fg(color::LightCyan),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::LightGreen => {
                print_item!(
                    color::Fg(color::LightGreen),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::LightMagenta => {
                print_item!(
                    color::Fg(color::LightMagenta),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::LightRed => {
                print_item!(
                    color::Fg(color::LightRed),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::LightWhite => {
                print_item!(
                    color::Fg(color::LightWhite),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::LightYellow => {
                print_item!(
                    color::Fg(color::LightYellow),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::Magenta => {
                print_item!(color::Fg(color::Magenta), name, time, selected, self.layout);
            }
            Colorname::Red => {
                print_item!(color::Fg(color::Red), name, time, selected, self.layout);
            }
            Colorname::Rgb(x, y, z) => {
                print_item!(
                    color::Fg(color::Rgb(*x, *y, *z)),
                    name,
                    time,
                    selected,
                    self.layout
                );
            }
            Colorname::White => {
                print_item!(color::Fg(color::White), name, time, selected, self.layout);
            }
            Colorname::Yellow => {
                print_item!(color::Fg(color::Yellow), name, time, selected, self.layout);
            }
        }
    }

    pub fn list_up(&self, skip_number: u16) {
        let row = self.layout.terminal_row;

        //if list exceeds max-row
        let mut row_count = 0;
        for (i, item) in self.list.iter().enumerate() {
            if i < skip_number as usize || (!self.show_hidden && item.is_hidden) {
                continue;
            }

            print!(
                "{}",
                cursor::Goto(3, i as u16 + STARTING_POINT - skip_number)
            );

            if row_count == row - STARTING_POINT {
                break;
            } else {
                self.print(i);
                row_count += 1;
            }
        }
    }

    pub fn update_list(&mut self) -> Result<(), MyError> {
        self.list = push_items(&self.current_dir, &self.sort_by, self.show_hidden)?;
        Ok(())
    }

    pub fn reset_selection(&mut self) {
        for mut item in self.list.iter_mut() {
            item.selected = false;
        }
    }

    pub fn select_from_top(&mut self, start_pos: usize) {
        for (i, item) in self.list.iter_mut().enumerate() {
            if i <= start_pos {
                item.selected = true;
            } else {
                item.selected = false;
            }
        }
    }

    pub fn select_to_bottom(&mut self, start_pos: usize) {
        for (i, item) in self.list.iter_mut().enumerate() {
            if i < start_pos {
                item.selected = false;
            } else {
                item.selected = true;
            }
        }
    }

    pub fn move_cursor(&mut self, nums: &Num, y: u16) {
        print!(" {}", cursor::Goto(1, self.layout.terminal_row));
        print!("{}", clear::CurrentLine);

        let item = self.get_item(nums.index);
        if let Ok(item) = item {
            match &item.file_ext {
                Some(ext) => {
                    print!(
                        "[{}/{}] {} {}",
                        nums.index + 1,
                        self.list.len(),
                        ext.clone().into_string().unwrap_or_default(),
                        to_proper_size(item.file_size)
                    );
                }
                None => {
                    print!(
                        "[{}/{}] {}",
                        nums.index + 1,
                        self.list.len(),
                        to_proper_size(item.file_size)
                    );
                }
            }
            if self.rust_log.is_some() {
                print!(
                    " index:{} skip:{} column:{} row:{}",
                    nums.index, nums.skip, self.layout.terminal_column, self.layout.terminal_row
                );
            }
        }
        print!("{}>{}", cursor::Goto(1, y), cursor::Left(1));
        self.layout.y = y;
    }

    pub fn write_session(&self, session_path: PathBuf) -> Result<(), MyError> {
        let session = Session {
            sort_by: self.sort_by.clone(),
            show_hidden: self.show_hidden,
        };
        let serialized = toml::to_string(&session)?;
        fs::write(&session_path, serialized)?;
        Ok(())
    }
}

fn make_item(entry: fs::DirEntry) -> ItemInfo {
    let path = entry.path();
    let metadata = fs::symlink_metadata(&path);

    let name = entry
        .file_name()
        .into_string()
        .unwrap_or_else(|_| "Invalid unicode name".to_string());

    let hidden = matches!(name.chars().next(), Some('.'));

    let ext = path.extension().map(|s| s.to_os_string());

    match metadata {
        Ok(metadata) => {
            let time = {
                let sometime = metadata.modified().unwrap();
                let chrono_time: DateTime<Local> = DateTime::from(sometime);
                Some(chrono_time.to_rfc3339_opts(SecondsFormat::Secs, false))
            };

            let filetype = {
                let file_type = metadata.file_type();
                if file_type.is_dir() {
                    FileType::Directory
                } else if file_type.is_file() {
                    FileType::File
                } else if file_type.is_symlink() {
                    FileType::Symlink
                } else {
                    FileType::File
                }
            };

            let sym_dir_path = {
                if filetype == FileType::Symlink {
                    if let Ok(sym_meta) = fs::metadata(&path) {
                        if sym_meta.is_dir() {
                            fs::canonicalize(path.clone()).ok()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            let size = metadata.len();
            ItemInfo {
                file_type: filetype,
                file_name: name,
                file_path: path,
                symlink_dir_path: sym_dir_path,
                file_size: size,
                file_ext: ext,
                modified: time,
                selected: false,
                is_hidden: hidden,
            }
        }
        Err(_) => ItemInfo {
            file_type: FileType::File,
            file_name: name,
            file_path: path,
            symlink_dir_path: None,
            file_size: 0,
            file_ext: ext,
            modified: None,
            selected: false,
            is_hidden: false,
        },
    }
}

pub fn push_items(p: &Path, key: &SortKey, show_hidden: bool) -> Result<Vec<ItemInfo>, MyError> {
    let mut result = Vec::new();
    let mut dir_v = Vec::new();
    let mut file_v = Vec::new();

    for entry in fs::read_dir(p)? {
        let e = entry?;
        let entry = make_item(e);
        match entry.file_type {
            FileType::Directory => dir_v.push(entry),
            FileType::File | FileType::Symlink => file_v.push(entry),
        }
    }

    match key {
        SortKey::Name => {
            dir_v.sort_by(|a, b| natord::compare(&a.file_name, &b.file_name));
            file_v.sort_by(|a, b| natord::compare(&a.file_name, &b.file_name));
        }
        SortKey::Time => {
            dir_v.sort_by(|a, b| b.modified.partial_cmp(&a.modified).unwrap());
            file_v.sort_by(|a, b| b.modified.partial_cmp(&a.modified).unwrap());
        }
    }

    result.append(&mut dir_v);
    result.append(&mut file_v);

    if !show_hidden {
        result.retain(|x| !x.is_hidden);
    }

    Ok(result)
}

pub fn trash_to_info(trash_dir: &PathBuf, vec: Vec<PathBuf>) -> Result<Vec<ItemInfo>, MyError> {
    let total = vec.len();
    let mut count = 0;
    let mut result = Vec::new();
    for entry in fs::read_dir(trash_dir)? {
        let entry = entry?;
        if vec.contains(&entry.path()) {
            result.push(make_item(entry));
            count += 1;
            if count == total {
                break;
            }
        }
    }
    Ok(result)
}
