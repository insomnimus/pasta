use std::{
	ffi::OsString,
	io::{
		self,
		Error,
		Read,
	},
	mem,
	os::windows::{
		ffi::OsStringExt,
		io::{
			AsRawHandle,
			IntoRawHandle,
		},
	},
	path::Path,
	process::Command,
	sync::atomic::{
		AtomicIsize,
		Ordering,
	},
};

use anyhow::{
	anyhow,
	ensure,
	Result,
};
use windows::{
	core::PCWSTR,
	Win32::{
		Foundation::{
			CloseHandle,
			BOOL,
			HANDLE,
			HINSTANCE,
			HWND,
			LPARAM,
			WPARAM,
		},
		Storage::FileSystem::GetFileType,
		System::{
			DataExchange::{
				CloseClipboard,
				GetClipboardData,
				IsClipboardFormatAvailable,
				OpenClipboard,
			},
			Memory::{
				GlobalLock,
				GlobalUnlock,
			},
			ProcessStatus::{
				K32EnumProcessModulesEx,
				K32EnumProcesses,
				K32GetModuleFileNameExW,
				LIST_MODULES_ALL,
			},
			SystemServices::CF_UNICODETEXT,
			Threading::{
				OpenProcess,
				WaitForInputIdle,
				PROCESS_QUERY_INFORMATION,
				PROCESS_VM_READ,
			},
		},
		UI::WindowsAndMessaging::{
			EnumWindows,
			FindWindowExW,
			GetWindowThreadProcessId,
			SendMessageW,
			SetForegroundWindow,
			WM_SETTEXT,
		},
	},
};

enum Data {
	Ptr(*const u16),
	Vec(Vec<u16>),
}

unsafe fn notepad_handle() -> io::Result<(HANDLE, u32)> {
	match find_notepad()? {
		Some(x) => Ok(x),
		None => {
			let child = Command::new("notepad.exe").spawn()?;
			let pid = child.id();
			Ok((HANDLE(child.into_raw_handle() as isize), pid))
		}
	}
}

unsafe fn get_hwnd(pid: u32) -> Option<HWND> {
	static NOTEPAD: AtomicIsize = AtomicIsize::new(0);

	unsafe extern "system" fn callback(hwnd: HWND, pid: LPARAM) -> BOOL {
		let mut out = 0_u32;
		GetWindowThreadProcessId(hwnd, &mut out as *mut u32);
		if out == pid.0 as u32 {
			NOTEPAD.store(hwnd.0, Ordering::Relaxed);
			false.into()
		} else {
			true.into()
		}
	}

	EnumWindows(Some(callback), LPARAM(pid as isize));
	let notepad = NOTEPAD.load(Ordering::Relaxed);
	if notepad != 0 {
		Some(HWND(notepad))
	} else {
		None
	}
}

fn is_stdin_tty() -> bool {
	// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfiletype
	static FILE_TYPE_CHAR: u32 = 0x0002;
	let handle = HANDLE(io::stdin().as_raw_handle() as isize);
	unsafe { GetFileType(handle) == FILE_TYPE_CHAR }
}

fn send_text(notepad_hwnd: HWND, data: &Data) -> Result<isize> {
	// NOTE: new notepad uses RichEditD2DPT
	let text = "Edit\0".encode_utf16().collect::<Vec<_>>();
	unsafe {
		let hwnd = FindWindowExW(Some(notepad_hwnd), None, Some(PCWSTR(text.as_ptr())), None);
		ensure!(hwnd.0 != 0, "no edit window found");

		let ptr = match data {
			Data::Vec(v) => v.as_ptr(),
			Data::Ptr(p) => *p,
		};

		Ok(SendMessageW(hwnd, WM_SETTEXT, WPARAM::default(), LPARAM(ptr as isize)).0)
	}
}

fn get_text_data() -> io::Result<Data> {
	if !is_stdin_tty() {
		let mut buf = String::new();
		io::stdin().lock().read_to_string(&mut buf)?;
		buf += "\0";
		return Ok(Data::Vec(buf.encode_utf16().collect()));
	}

	unsafe {
		if !IsClipboardFormatAvailable(CF_UNICODETEXT.0).as_bool() {
			return Ok(Data::Vec(vec![0]));
		}
		if OpenClipboard(None).0 == 0 {
			return Err(Error::last_os_error());
		}
		let handle = GetClipboardData(CF_UNICODETEXT.0)?;
		if handle.is_invalid() {
			return Err(Error::last_os_error());
		}
		let lock = GlobalLock(handle.0).cast::<u16>();
		if lock.is_null() {
			return Err(Error::last_os_error());
		}
		Ok(Data::Ptr(lock))
	}
}

unsafe fn find_notepad() -> io::Result<Option<(HANDLE, u32)>> {
	let mut pids = vec![0_u32; 1024];
	let len = pids.len();
	let mut n_bytes = 0_u32;
	let res = K32EnumProcesses(pids.as_mut_ptr(), len as u32 * 4, &mut n_bytes as *mut _);

	if res.0 == 0 {
		return Err(Error::last_os_error());
	}

	pids.truncate(n_bytes as usize / 4);

	for &pid in &pids {
		if pid == 0 {
			continue;
		}
		let handle = match OpenProcess(
			PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
			BOOL::from(false),
			pid,
		) {
			Ok(x) if !x.is_invalid() => x,
			_ => continue,
		};

		let mut mods = vec![HINSTANCE(0); 1024];
		let size = mods.len() * mem::size_of::<HINSTANCE>();
		let mut n_bytes = 0_u32;
		let res = K32EnumProcessModulesEx(
			handle,
			mods.as_ptr() as *mut _,
			size as _,
			&mut n_bytes as *mut u32,
			LIST_MODULES_ALL,
		);
		if !res.as_bool() {
			return Err(Error::last_os_error());
		}
		mods.truncate(n_bytes as usize / mem::size_of::<HINSTANCE>());
		if mods.is_empty() {
			continue;
		}

		let mut buf = vec![0_u16; 1024];
		let res = K32GetModuleFileNameExW(handle, mods[0], &mut buf);
		if res == 0 {
			return Err(Error::last_os_error());
		}
		buf.truncate(res as usize);
		let buf = buf;
		let path = OsString::from_wide(&buf);
		let path = Path::new(&path);
		if path
			.file_name()
			.map_or(false, |s| s.eq_ignore_ascii_case("notepad.exe"))
		{
			return Ok(Some((handle, pid)));
		}
		CloseHandle(handle);
	}

	Ok(None)
}

fn main() -> Result<()> {
	unsafe fn run() -> Result<()> {
		let (handle, pid) = notepad_handle()?;
		let code = WaitForInputIdle(handle, 2500);
		ensure!(
			code == 0,
			"failed waiting for notepad window: code = {code}"
		);
		let hwnd = get_hwnd(pid).ok_or_else(|| anyhow!("could not locate a notepad window"))?;

		ensure!(
			SetForegroundWindow(hwnd).as_bool(),
			"failed to focus on notepad"
		);

		let data = get_text_data()?;
		send_text(hwnd, &data)?;

		if let Data::Ptr(p) = data {
			if !GlobalUnlock(p as isize).as_bool() {
				return Err(Error::last_os_error().into());
			}

			if !CloseClipboard().as_bool() {
				return Err(Error::last_os_error().into());
			}
		}

		CloseHandle(handle);
		Ok(())
	}

	unsafe { run() }
}
