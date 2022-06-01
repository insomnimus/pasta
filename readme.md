# Pasta
Pipe text to notepad!

This repo exists because I wanted to get familiar with the raw Win32 API (in Rust).
The code tries to use as many Win32 functions as possible (It could be more, I'm on it).

So, it only works on Windows.

## Usage
```sh
# Pipe to it
echo "Hello, notepad!" | pasta
# Or, call it bare to use the clipboard contents
pasta
```
