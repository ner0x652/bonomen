#[macro_use]
extern crate clap;
extern crate log;
extern crate strsim;
extern crate term;

#[cfg(windows)]
extern crate winapi;
#[cfg(windows)]
extern crate kernel32;

#[cfg(unix)]
extern crate psutil;
#[cfg(unix)]
extern crate libc;

use clap::{Arg, App};

#[cfg(unix)]
use psutil::process::Process;
use strsim::damerau_levenshtein;
use std::fs::File;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::io::{BufRead, BufReader, Write, stdout};
#[cfg(unix)]
use std::process::exit;
#[cfg(windows)]
use std::mem::size_of;
#[cfg(windows)]
use std::ptr;

#[cfg(windows)]
use winapi::winnt::PROCESS_QUERY_INFORMATION;
#[cfg(windows)]
use winapi::winnt::PROCESS_VM_READ;
#[cfg(windows)]
use winapi::minwindef::HMODULE;
#[cfg(windows)]
use winapi::minwindef::DWORD;
#[cfg(windows)]
use winapi::minwindef::FALSE;
#[cfg(windows)]
use winapi::psapi::LIST_MODULES_ALL;

#[cfg(windows)]
use kernel32::OpenProcess;
#[cfg(windows)]
use kernel32::K32EnumProcessModulesEx;
#[cfg(windows)]
use kernel32::K32GetModuleBaseNameW;
#[cfg(windows)]
use kernel32::K32EnumProcesses;
#[cfg(windows)]
use kernel32::K32GetModuleFileNameExW;

mod types;

const BONOMEN_BANNER: &'static str = r"
      =======  ======= ==    == ======= ========== ====== ==    ==
      ||   //  ||   || ||\\  || ||   || ||\\  //|| ||     ||\\  ||
      ||====   ||   || || \\ || ||   || ||  ||  || ||==== || \\ ||
      ||   \\  ||   || ||  \\|| ||   || ||  ||  || ||     ||  \\||
      =======  ======= ==    == ======= ==  ==  == ====== ==    ==";

const DEFAULT_FILE: &'static str = "default_procs.txt";

fn main() {
    // Handle command line arguments
    let matches = App::new(BONOMEN_BANNER)
        .version(crate_version!())
        .author(crate_authors!())
        .about("Detect critical process impersonation")
        .arg(Arg::with_name("file")
             .short("f")
             .long("file")
             .value_name("FILE")
             .help("File containing critical processes path, threshold, whitelist")
             .takes_value(true))
        .arg(Arg::with_name("verbose")
             .short("v")
             .long("verbose")
             .help("Verbose mode"))
        .get_matches();

    let mut terminal = term::stdout().unwrap();
    if terminal.supports_attr(term::Attr::Bold) {
        match terminal.attr(term::Attr::Bold) {
            Ok(ok)   => ok,
            Err(why) => println!("{}", why.to_string()),
        }
    }

    println!("{}\n\tAuthor(s):{} Version:{}\n",
             BONOMEN_BANNER, crate_authors!(), crate_version!());
    terminal.reset().unwrap();

    #[cfg(unix)]
    unsafe {
       	if libc::geteuid() != 0 {
            terminal.attr(term::Attr::Bold).unwrap();
            terminal.fg(term::color::RED).unwrap();
            println!("{}", "BONOMEN needs root privileges to read process executable path!");
            terminal.reset().unwrap();
            let _ = stdout().flush();
            
            exit(0);
        }
    };

    let file_name = matches.value_of("file").unwrap_or(DEFAULT_FILE);
    let verb_mode = if matches.is_present("verbose") { true } else { false };

    // Load known standard system processes
    terminal.fg(term::color::GREEN).unwrap();
    println!("Standard processes file: {}", file_name);
    terminal.reset().unwrap();
    let crit_proc_vec = read_procs_file(&file_name);

    let r;

    #[cfg(unix)] {
        // Read current active processes
        let sys_procs_vec = read_unix_system_procs();
        // Check for process name impersonation
        r = unix_check_procs_impers(&crit_proc_vec, &sys_procs_vec, &verb_mode, &mut terminal);
    }

    #[cfg(windows)] {
        let sys_procs_vec = read_win_system_procs(&mut terminal);

        r = win_check_procs_impers(&crit_proc_vec, &sys_procs_vec, &verb_mode, &mut terminal);
    }

    if r > 0 {
        terminal.fg(term::color::RED).unwrap();
    } else {
        terminal.fg(term::color::GREEN).unwrap();
    }
    println!("Found {} suspicious processes.\n{}", r, "Done!");
    terminal.reset().unwrap();
    let _ = stdout().flush();
}

// Read standard system processes from a file.
// Each line in the file is of the format:
// <process name>:<threshold value>:<process absolute path>
fn read_procs_file(file_name: &str) -> Vec<types::ProcProps> {
    let path    = Path::new(file_name);
    let display = path.display();

    let file = match File::open(&path) {
        Err(why) => panic!("couldn't open {}: {}", display, why.to_string()),
        Ok(file) => file,
    };

    let mut procs = Vec::new();

    // Read whole file line by line, and unwrap each line
    let reader = BufReader::new(file);
    let lines  = reader.lines().map(|l| l.unwrap());

    for line in lines {
        // Split each line into a vector
        let v: Vec<_> = line.split(';').map(|s| s.to_string()).collect();
        assert!(v.len() >= 3, "Invalid format, line: {}", line);
        let mut wl    = Vec::new();

        // Push process absolute path, may be more than 1 path
        for i in 2 .. v.len() {
            wl.push(v[i].to_string());
        }

        procs.push(types::ProcProps {
            name:      v[0].to_string(),
            threshold: v[1].parse::<u32>().unwrap(),
            whitelist: wl
        });
    }

    procs
}

fn is_whitelisted(proc_path: &str, whitelist: &Vec<std::string::String>) -> bool {
    whitelist.iter().any(|p| p == proc_path)
}

// Read running processes
#[cfg(unix)]
fn read_unix_system_procs() -> Vec<Process> {
    psutil::process::all().unwrap()
}

#[cfg(windows)]
fn read_win_system_procs(terminal: &mut Box<term::StdoutTerminal>) -> Vec<types::WinProc> {
    let mut win_procs = Vec::new();

    const SIZE: usize = 1024;
    let mut pids = [0; SIZE];
    let mut written = 0;
    unsafe {
        if K32EnumProcesses(pids.as_mut_ptr(), (pids.len() * size_of::<DWORD>()) as u32, &mut written) == 0 {
            terminal.fg(term::color::RED).unwrap();
            println!("{}", "K32EnumProcesses failed!");
            terminal.reset().unwrap();

            return win_procs;
        }
    }
    let processes = &pids[..(written / size_of::<DWORD>() as u32) as usize]; // Slice trick thanks to WindowsBunny @ #rust

    const NAME_SZ: usize = 64;
    let mut sz_process_name = [0; NAME_SZ];
    const PATH_SZ: usize = 254;
    let mut sz_process_path = [0; PATH_SZ];
    
    for i in 0 .. processes.len() {
        let process_id: DWORD = processes[i];
        unsafe {
            let h_process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, FALSE, process_id);
	    
            
            if !h_process.is_null() {
                let h_mod     = ptr::null_mut();
                let cb_needed = ptr::null_mut();
	        
                if K32EnumProcessModulesEx(h_process, h_mod, size_of::<HMODULE>() as u32, cb_needed, LIST_MODULES_ALL) > 0 {
                    terminal.fg(term::color::RED).unwrap();
                    println!("PID: {} {}", process_id, "K32EnumProcessModules failed!");
                    terminal.reset().unwrap();

                    continue;
                } else {
                    if K32GetModuleBaseNameW(h_process, *h_mod, sz_process_name.as_mut_ptr(), NAME_SZ as u32) == 0 {
                        terminal.fg(term::color::RED).unwrap();
                        println!("PID: {} {}", process_id, "K32GetModuleBaseNameW failed!");
                        terminal.reset().unwrap();

                        continue;
                    } else {
                        if K32GetModuleFileNameExW(h_process, *h_mod, sz_process_path.as_mut_ptr(), PATH_SZ as u32) == 0 {
                            terminal.fg(term::color::RED).unwrap();
                            println!("PID: {} {}", process_id, "K32GetModuleFileNameExW failed!");
                            terminal.reset().unwrap();

                            continue;
                        }
                    }
                }
            }
	}

        let name_str = String::from_utf16(&sz_process_name[..])
            .unwrap()
            .split('\u{0}')
            .next()
            .unwrap_or("")
            .to_string();
        let path_str = String::from_utf16(&sz_process_path[..])
            .unwrap()
            .split('\u{0}')
            .next()
            .unwrap_or("")
            .to_string();

        if name_str != "" && path_str != "" {
            win_procs.push(types::WinProc {
                name    : name_str,
                exe_path: path_str
            });
        }
    }

    win_procs
}

#[cfg(windows)]
fn win_check_procs_impers(crit_procs_vec: &Vec<types::ProcProps>,
                          sys_procs_vec : &Vec<types::WinProc>,
                          verb_mode     : &bool,
                          terminal      : &mut Box<term::StdoutTerminal>) -> u32 {
    let mut susp_procs: u32 = 0;

    for sys_proc in sys_procs_vec.iter() {
        if *verb_mode {
            terminal.fg(term::color::BRIGHT_GREEN).unwrap();
            println!("> Checking system process: {}", sys_proc.name);
            println!("> system process executable absolute path: {}", sys_proc.exe_path);
        }

        for crit_proc in crit_procs_vec.iter() {
            let threshold = damerau_levenshtein(&sys_proc.name, &crit_proc.name);
            if *verb_mode {
                terminal.fg(term::color::CYAN).unwrap();
                println!( "\tagainst critical process: {}, distance: {}", crit_proc.name, threshold);
                terminal.reset().unwrap();
            }

            if threshold > 0 && threshold <= crit_proc.threshold as usize &&
                !is_whitelisted(&sys_proc.exe_path, &crit_proc.whitelist) {
                    terminal.fg(term::color::RED).unwrap();
                    println!("Suspicious: {} <-> {} : distance {}", sys_proc.name, crit_proc.name, threshold);
                    terminal.reset().unwrap();

                    susp_procs += 1;
            }
        }
    }

    susp_procs
}

#[cfg(unix)]
fn unix_check_procs_impers(crit_procs_vec: &Vec<types::ProcProps>,
                           sys_procs_vec : &Vec<Process>,
                           verb_mode     : &bool,
                           terminal      : &mut Box<term::StdoutTerminal>) -> u32 {
    // Number of suspicious processes
    let mut susp_procs: u32 = 0;

    for sys_proc in sys_procs_vec.iter() {
        let exe_path = match sys_proc.exe() {
            Ok(path) => path,
            Err(why) => PathBuf::from(why.to_string()),
        };

        if *verb_mode {
            terminal.fg(term::color::BRIGHT_GREEN).unwrap();
            println!("> Checking system process: {}", sys_proc.comm);
            println!("> system process executable absolute path: {}", exe_path.to_str().unwrap());
        }

        for crit_proc in crit_procs_vec.iter() {
            let threshold = damerau_levenshtein(&sys_proc.comm, &crit_proc.name);
            if *verb_mode {
                terminal.fg(term::color::CYAN).unwrap();
                println!( "\tagainst critical process: {}, distance: {}", crit_proc.name, threshold);
                terminal.reset().unwrap();
            }

            if threshold > 0 && threshold <= crit_proc.threshold as usize &&
                !is_whitelisted(&(exe_path.to_str().unwrap()), &crit_proc.whitelist) {
                    terminal.fg(term::color::RED).unwrap();
                    println!("Suspicious: {} <-> {} : distance {}", sys_proc.comm, crit_proc.name, threshold);
                    terminal.reset().unwrap();

                    susp_procs += 1;
            }
        }
    }

    susp_procs
}
