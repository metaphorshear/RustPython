/*
 * I/O core tools.
 */
use std::cell::{RefCell, RefMut};
use std::fs;
use std::io::prelude::*;
use std::io::Cursor;
use std::io::SeekFrom;

use num_traits::ToPrimitive;

use crate::function::{OptionalArg, OptionalOption, PyFuncArgs};
use crate::obj::objbool;
use crate::obj::objbytearray::PyByteArray;
use crate::obj::objbyteinner::PyBytesLike;
use crate::obj::objbytes;
use crate::obj::objint;
use crate::obj::objiter;
use crate::obj::objstr::{self, PyStringRef};
use crate::obj::objtype::{self, PyClassRef};
use crate::pyobject::{
    BufferProtocol, Either, PyObjectRef, PyRef, PyResult, PyValue, TryFromObject,
};
use crate::vm::VirtualMachine;

fn byte_count(bytes: OptionalOption<i64>) -> i64 {
    bytes.flat_option().unwrap_or(-1 as i64)
}

const DEFAULT_BUFFER_SIZE: usize = 8 * 1024;

#[derive(Debug)]
struct BufferedIO {
    cursor: Cursor<Vec<u8>>,
}

impl BufferedIO {
    fn new(cursor: Cursor<Vec<u8>>) -> BufferedIO {
        BufferedIO { cursor }
    }

    fn write(&mut self, data: &[u8]) -> Option<u64> {
        let length = data.len();

        match self.cursor.write_all(data) {
            Ok(_) => Some(length as u64),
            Err(_) => None,
        }
    }

    //return the entire contents of the underlying
    fn getvalue(&self) -> Vec<u8> {
        self.cursor.clone().into_inner()
    }

    //skip to the jth position
    fn seek(&mut self, offset: u64) -> Option<u64> {
        match self.cursor.seek(SeekFrom::Start(offset)) {
            Ok(_) => Some(offset),
            Err(_) => None,
        }
    }

    //Read k bytes from the object and return.
    fn read(&mut self, bytes: i64) -> Option<Vec<u8>> {
        let mut buffer = Vec::new();

        //for a defined number of bytes, i.e. bytes != -1
        if bytes > 0 {
            let mut handle = self.cursor.clone().take(bytes as u64);
            //read handle into buffer

            if handle.read_to_end(&mut buffer).is_err() {
                return None;
            }
            //the take above consumes the struct value
            //we add this back in with the takes into_inner method
            self.cursor = handle.into_inner();
        } else {
            //read handle into buffer
            if self.cursor.read_to_end(&mut buffer).is_err() {
                return None;
            }
        };

        Some(buffer)
    }

    fn tell(&self) -> u64 {
        self.cursor.position()
    }

    fn readline(&mut self) -> Option<String> {
        let mut buf = String::new();

        match self.cursor.read_line(&mut buf) {
            Ok(_) => Some(buf),
            Err(_) => None,
        }
    }
}

#[derive(Debug)]
struct PyStringIO {
    buffer: RefCell<Option<BufferedIO>>,
}

type PyStringIORef = PyRef<PyStringIO>;

impl PyValue for PyStringIO {
    fn class(vm: &VirtualMachine) -> PyClassRef {
        vm.class("io", "StringIO")
    }
}

impl PyStringIORef {
    fn buffer(&self, vm: &VirtualMachine) -> PyResult<RefMut<BufferedIO>> {
        let buffer = self.buffer.borrow_mut();
        if buffer.is_some() {
            Ok(RefMut::map(buffer, |opt| opt.as_mut().unwrap()))
        } else {
            Err(vm.new_value_error("I/O operation on closed file.".to_string()))
        }
    }

    //write string to underlying vector
    fn write(self, data: PyStringRef, vm: &VirtualMachine) -> PyResult {
        let bytes = data.as_str().as_bytes();

        match self.buffer(vm)?.write(bytes) {
            Some(value) => Ok(vm.ctx.new_int(value)),
            None => Err(vm.new_type_error("Error Writing String".to_string())),
        }
    }

    //return the entire contents of the underlying
    fn getvalue(self, vm: &VirtualMachine) -> PyResult {
        match String::from_utf8(self.buffer(vm)?.getvalue()) {
            Ok(result) => Ok(vm.ctx.new_str(result)),
            Err(_) => Err(vm.new_value_error("Error Retrieving Value".to_string())),
        }
    }

    //skip to the jth position
    fn seek(self, offset: u64, vm: &VirtualMachine) -> PyResult {
        match self.buffer(vm)?.seek(offset) {
            Some(value) => Ok(vm.ctx.new_int(value)),
            None => Err(vm.new_value_error("Error Performing Operation".to_string())),
        }
    }

    fn seekable(self, _vm: &VirtualMachine) -> bool {
        true
    }

    //Read k bytes from the object and return.
    //If k is undefined || k == -1, then we read all bytes until the end of the file.
    //This also increments the stream position by the value of k
    fn read(self, bytes: OptionalOption<i64>, vm: &VirtualMachine) -> PyResult {
        let data = match self.buffer(vm)?.read(byte_count(bytes)) {
            Some(value) => value,
            None => Vec::new(),
        };

        match String::from_utf8(data) {
            Ok(value) => Ok(vm.ctx.new_str(value)),
            Err(_) => Err(vm.new_value_error("Error Retrieving Value".to_string())),
        }
    }

    fn tell(self, vm: &VirtualMachine) -> PyResult<u64> {
        Ok(self.buffer(vm)?.tell())
    }

    fn readline(self, vm: &VirtualMachine) -> PyResult<String> {
        match self.buffer(vm)?.readline() {
            Some(line) => Ok(line),
            None => Err(vm.new_value_error("Error Performing Operation".to_string())),
        }
    }

    fn truncate(self, size: OptionalOption<usize>, vm: &VirtualMachine) -> PyResult<()> {
        let mut buffer = self.buffer(vm)?;
        let size = size.flat_option().unwrap_or_else(|| buffer.tell() as usize);
        buffer.cursor.get_mut().truncate(size);
        Ok(())
    }

    fn closed(self, _vm: &VirtualMachine) -> bool {
        self.buffer.borrow().is_none()
    }

    fn close(self, _vm: &VirtualMachine) {
        self.buffer.replace(None);
    }
}

#[derive(FromArgs)]
struct StringIOArgs {
    #[pyarg(positional_or_keyword, default = "None")]
    #[allow(dead_code)]
    // TODO: use this
    newline: Option<PyStringRef>,
}

fn string_io_new(
    cls: PyClassRef,
    object: OptionalArg<Option<PyObjectRef>>,
    _args: StringIOArgs,
    vm: &VirtualMachine,
) -> PyResult<PyStringIORef> {
    let raw_string = match object {
        OptionalArg::Present(Some(ref input)) => objstr::get_value(input),
        _ => String::new(),
    };

    PyStringIO {
        buffer: RefCell::new(Some(BufferedIO::new(Cursor::new(raw_string.into_bytes())))),
    }
    .into_ref_with_type(vm, cls)
}

#[derive(Debug)]
struct PyBytesIO {
    buffer: RefCell<Option<BufferedIO>>,
}

type PyBytesIORef = PyRef<PyBytesIO>;

impl PyValue for PyBytesIO {
    fn class(vm: &VirtualMachine) -> PyClassRef {
        vm.class("io", "BytesIO")
    }
}

impl PyBytesIORef {
    fn buffer(&self, vm: &VirtualMachine) -> PyResult<RefMut<BufferedIO>> {
        let buffer = self.buffer.borrow_mut();
        if buffer.is_some() {
            Ok(RefMut::map(buffer, |opt| opt.as_mut().unwrap()))
        } else {
            Err(vm.new_value_error("I/O operation on closed file.".to_string()))
        }
    }

    fn write(self, data: PyBytesLike, vm: &VirtualMachine) -> PyResult<u64> {
        let mut buffer = self.buffer(vm)?;
        match data.with_ref(|b| buffer.write(b)) {
            Some(value) => Ok(value),
            None => Err(vm.new_type_error("Error Writing Bytes".to_string())),
        }
    }
    //Retrieves the entire bytes object value from the underlying buffer
    fn getvalue(self, vm: &VirtualMachine) -> PyResult {
        Ok(vm.ctx.new_bytes(self.buffer(vm)?.getvalue()))
    }

    //Takes an integer k (bytes) and returns them from the underlying buffer
    //If k is undefined || k == -1, then we read all bytes until the end of the file.
    //This also increments the stream position by the value of k
    fn read(self, bytes: OptionalOption<i64>, vm: &VirtualMachine) -> PyResult {
        match self.buffer(vm)?.read(byte_count(bytes)) {
            Some(value) => Ok(vm.ctx.new_bytes(value)),
            None => Err(vm.new_value_error("Error Retrieving Value".to_string())),
        }
    }

    //skip to the jth position
    fn seek(self, offset: u64, vm: &VirtualMachine) -> PyResult {
        match self.buffer(vm)?.seek(offset) {
            Some(value) => Ok(vm.ctx.new_int(value)),
            None => Err(vm.new_value_error("Error Performing Operation".to_string())),
        }
    }

    fn seekable(self, _vm: &VirtualMachine) -> bool {
        true
    }

    fn tell(self, vm: &VirtualMachine) -> PyResult<u64> {
        Ok(self.buffer(vm)?.tell())
    }

    fn readline(self, vm: &VirtualMachine) -> PyResult<Vec<u8>> {
        match self.buffer(vm)?.readline() {
            Some(line) => Ok(line.as_bytes().to_vec()),
            None => Err(vm.new_value_error("Error Performing Operation".to_string())),
        }
    }

    fn truncate(self, size: OptionalOption<usize>, vm: &VirtualMachine) -> PyResult<()> {
        let mut buffer = self.buffer(vm)?;
        let size = size.flat_option().unwrap_or_else(|| buffer.tell() as usize);
        buffer.cursor.get_mut().truncate(size);
        Ok(())
    }

    fn closed(self, _vm: &VirtualMachine) -> bool {
        self.buffer.borrow().is_none()
    }

    fn close(self, _vm: &VirtualMachine) {
        self.buffer.replace(None);
    }
}

fn bytes_io_new(
    cls: PyClassRef,
    object: OptionalArg<Option<PyObjectRef>>,
    vm: &VirtualMachine,
) -> PyResult<PyBytesIORef> {
    let raw_bytes = match object {
        OptionalArg::Present(Some(ref input)) => objbytes::get_value(input).to_vec(),
        _ => vec![],
    };

    PyBytesIO {
        buffer: RefCell::new(Some(BufferedIO::new(Cursor::new(raw_bytes)))),
    }
    .into_ref_with_type(vm, cls)
}

fn io_base_cm_enter(instance: PyObjectRef, _vm: &VirtualMachine) -> PyObjectRef {
    instance.clone()
}

fn io_base_cm_exit(instance: PyObjectRef, _args: PyFuncArgs, vm: &VirtualMachine) -> PyResult<()> {
    vm.call_method(&instance, "close", vec![])?;
    Ok(())
}

// TODO Check if closed, then if so raise ValueError
fn io_base_flush(_self: PyObjectRef, _vm: &VirtualMachine) {}

fn io_base_seekable(_self: PyObjectRef, _vm: &VirtualMachine) -> bool {
    false
}
fn io_base_readable(_self: PyObjectRef, _vm: &VirtualMachine) -> bool {
    false
}
fn io_base_writable(_self: PyObjectRef, _vm: &VirtualMachine) -> bool {
    false
}

fn io_base_closed(instance: PyObjectRef, vm: &VirtualMachine) -> PyResult {
    vm.get_attribute(instance, "__closed")
}

fn io_base_close(instance: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
    let closed = objbool::boolval(vm, io_base_closed(instance.clone(), vm)?)?;
    if !closed {
        let res = vm.call_method(&instance, "flush", vec![]);
        vm.set_attr(&instance, "__closed", vm.ctx.new_bool(true))?;
        res?;
    }
    Ok(())
}

fn io_base_readline(
    instance: PyObjectRef,
    size: OptionalOption<i64>,
    vm: &VirtualMachine,
) -> PyResult<Vec<u8>> {
    let size = byte_count(size);
    let mut res = Vec::<u8>::new();
    let read = vm.get_attribute(instance, "read")?;
    while size < 0 || res.len() < size as usize {
        let read_res = PyBytesLike::try_from_object(vm, vm.invoke(&read, vec![vm.new_int(1)])?)?;
        if read_res.with_ref(|b| b.is_empty()) {
            break;
        }
        read_res.with_ref(|b| res.extend_from_slice(b));
        if res.ends_with(b"\n") {
            break;
        }
    }
    Ok(res)
}

fn io_base_checkclosed(
    instance: PyObjectRef,
    msg: OptionalOption<PyObjectRef>,
    vm: &VirtualMachine,
) -> PyResult<()> {
    if objbool::boolval(vm, vm.get_attribute(instance, "closed")?)? {
        let msg = msg
            .flat_option()
            .unwrap_or_else(|| vm.new_str("I/O operation on closed file.".to_string()));
        Err(vm.new_exception(vm.ctx.exceptions.value_error.clone(), vec![msg]))
    } else {
        Ok(())
    }
}

fn io_base_checkreadable(
    instance: PyObjectRef,
    msg: OptionalOption<PyObjectRef>,
    vm: &VirtualMachine,
) -> PyResult<()> {
    if !objbool::boolval(vm, vm.call_method(&instance, "readable", vec![])?)? {
        let msg = msg
            .flat_option()
            .unwrap_or_else(|| vm.new_str("File or stream is not readable.".to_string()));
        Err(vm.new_exception(vm.ctx.exceptions.value_error.clone(), vec![msg]))
    } else {
        Ok(())
    }
}

fn io_base_checkwritable(
    instance: PyObjectRef,
    msg: OptionalOption<PyObjectRef>,
    vm: &VirtualMachine,
) -> PyResult<()> {
    if !objbool::boolval(vm, vm.call_method(&instance, "writable", vec![])?)? {
        let msg = msg
            .flat_option()
            .unwrap_or_else(|| vm.new_str("File or stream is not writable.".to_string()));
        Err(vm.new_exception(vm.ctx.exceptions.value_error.clone(), vec![msg]))
    } else {
        Ok(())
    }
}

fn io_base_checkseekable(
    instance: PyObjectRef,
    msg: OptionalOption<PyObjectRef>,
    vm: &VirtualMachine,
) -> PyResult<()> {
    if !objbool::boolval(vm, vm.call_method(&instance, "seekable", vec![])?)? {
        let msg = msg
            .flat_option()
            .unwrap_or_else(|| vm.new_str("File or stream is not seekable.".to_string()));
        Err(vm.new_exception(vm.ctx.exceptions.value_error.clone(), vec![msg]))
    } else {
        Ok(())
    }
}

fn io_base_iter(instance: PyObjectRef, _vm: &VirtualMachine) -> PyObjectRef {
    instance
}
fn io_base_next(instance: PyObjectRef, vm: &VirtualMachine) -> PyResult {
    let line = vm.call_method(&instance, "readline", vec![])?;
    if !objbool::boolval(vm, line.clone())? {
        Err(objiter::new_stop_iteration(vm))
    } else {
        Ok(line)
    }
}
fn io_base_readlines(instance: PyObjectRef, vm: &VirtualMachine) -> PyResult {
    Ok(vm.ctx.new_list(vm.extract_elements(&instance)?))
}

fn raw_io_base_read(
    instance: PyObjectRef,
    size: OptionalOption<i64>,
    vm: &VirtualMachine,
) -> PyResult {
    let size = byte_count(size);
    if size < 0 {
        return vm.call_method(&instance, "readall", vec![]);
    }
    let b = PyByteArray::new(vec![0; size as usize]).into_ref(vm);
    let n = <Option<usize>>::try_from_object(
        vm,
        vm.call_method(&instance, "readinto", vec![b.as_object().clone()])?,
    )?;
    if let Some(n) = n {
        let bytes = &mut b.inner.borrow_mut().elements;
        bytes.truncate(n);
        Ok(vm.ctx.new_bytes(bytes.clone()))
    } else {
        Ok(vm.get_none())
    }
}

fn buffered_io_base_init(
    instance: PyObjectRef,
    raw: PyObjectRef,
    buffer_size: OptionalArg<usize>,
    vm: &VirtualMachine,
) -> PyResult<()> {
    vm.set_attr(&instance, "raw", raw.clone())?;
    vm.set_attr(
        &instance,
        "buffer_size",
        vm.new_int(buffer_size.unwrap_or(DEFAULT_BUFFER_SIZE)),
    )?;
    Ok(())
}

fn buffered_io_base_fileno(instance: PyObjectRef, vm: &VirtualMachine) -> PyResult {
    let raw = vm.get_attribute(instance, "raw")?;
    vm.call_method(&raw, "fileno", vec![])
}

fn buffered_reader_read(
    instance: PyObjectRef,
    size: OptionalOption<i64>,
    vm: &VirtualMachine,
) -> PyResult {
    vm.call_method(
        &vm.get_attribute(instance.clone(), "raw")?,
        "read",
        vec![vm.new_int(byte_count(size))],
    )
}

fn buffered_reader_seekable(_self: PyObjectRef, _vm: &VirtualMachine) -> bool {
    true
}

fn buffered_reader_close(instance: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
    let raw = vm.get_attribute(instance, "raw")?;
    vm.invoke(&vm.get_attribute(raw, "close")?, vec![])?;
    Ok(())
}

// disable FileIO on WASM
#[cfg(not(target_arch = "wasm32"))]
mod fileio {
    use super::super::os;
    use super::*;

    fn compute_c_flag(mode: &str) -> u32 {
        let flag = match mode.chars().next() {
            Some(mode) => match mode {
                'w' => libc::O_WRONLY | libc::O_CREAT,
                'x' => libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                'a' => libc::O_APPEND,
                '+' => libc::O_RDWR,
                _ => libc::O_RDONLY,
            },
            None => libc::O_RDONLY,
        };
        flag as u32
    }

    fn file_io_init(
        file_io: PyObjectRef,
        name: Either<PyStringRef, i64>,
        mode: OptionalArg<PyStringRef>,
        vm: &VirtualMachine,
    ) -> PyResult {
        let (name, file_no) = match name {
            Either::A(name) => {
                let mode = match mode {
                    OptionalArg::Present(mode) => compute_c_flag(mode.as_str()),
                    OptionalArg::Missing => libc::O_RDONLY as _,
                };
                (
                    name.clone().into_object(),
                    os::os_open(
                        name,
                        mode as _,
                        OptionalArg::Missing,
                        OptionalArg::Missing,
                        vm,
                    )?,
                )
            }
            Either::B(fno) => (vm.new_int(fno), fno),
        };

        vm.set_attr(&file_io, "name", name)?;
        vm.set_attr(&file_io, "__fileno", vm.new_int(file_no))?;
        vm.set_attr(&file_io, "closefd", vm.new_bool(false))?;
        vm.set_attr(&file_io, "closed", vm.new_bool(false))?;
        Ok(vm.get_none())
    }

    fn fio_get_fileno(instance: &PyObjectRef, vm: &VirtualMachine) -> PyResult<fs::File> {
        io_base_checkclosed(instance.clone(), OptionalArg::Missing, vm)?;
        let fileno = i64::try_from_object(vm, vm.get_attribute(instance.clone(), "__fileno")?)?;
        Ok(os::rust_file(fileno))
    }
    fn fio_set_fileno(instance: &PyObjectRef, f: fs::File, vm: &VirtualMachine) -> PyResult<()> {
        let updated = os::raw_file_number(f);
        vm.set_attr(&instance, "__fileno", vm.ctx.new_int(updated))?;
        Ok(())
    }

    fn file_io_read(
        instance: PyObjectRef,
        read_byte: OptionalOption<i64>,
        vm: &VirtualMachine,
    ) -> PyResult<Vec<u8>> {
        let read_byte = byte_count(read_byte);

        let mut handle = fio_get_fileno(&instance, vm)?;

        let bytes = if read_byte < 0 {
            let mut bytes = vec![];
            handle
                .read_to_end(&mut bytes)
                .map_err(|e| os::convert_io_error(vm, e))?;
            bytes
        } else {
            let mut bytes = vec![0; read_byte as usize];
            let n = handle
                .read(&mut bytes)
                .map_err(|e| os::convert_io_error(vm, e))?;
            bytes.truncate(n);
            bytes
        };
        fio_set_fileno(&instance, handle, vm)?;

        Ok(bytes)
    }

    fn file_io_readinto(
        instance: PyObjectRef,
        obj: PyObjectRef,
        vm: &VirtualMachine,
    ) -> PyResult<()> {
        if !obj.readonly() {
            return Err(vm.new_type_error(
                "readinto() argument must be read-write bytes-like object".to_string(),
            ));
        }

        //extract length of buffer
        let py_length = vm.call_method(&obj, "__len__", PyFuncArgs::default())?;
        let length = objint::get_value(&py_length).to_u64().unwrap();

        let handle = fio_get_fileno(&instance, vm)?;

        let mut f = handle.take(length);
        if let Some(bytes) = obj.payload::<PyByteArray>() {
            //TODO: Implement for MemoryView

            let value_mut = &mut bytes.inner.borrow_mut().elements;
            value_mut.clear();
            match f.read_to_end(value_mut) {
                Ok(_) => {}
                Err(_) => return Err(vm.new_value_error("Error reading from Take".to_string())),
            }
        };

        fio_set_fileno(&instance, f.into_inner(), vm)?;

        Ok(())
    }

    fn file_io_write(
        instance: PyObjectRef,
        obj: PyBytesLike,
        vm: &VirtualMachine,
    ) -> PyResult<usize> {
        let mut handle = fio_get_fileno(&instance, vm)?;

        let len = obj
            .with_ref(|b| handle.write(b))
            .map_err(|e| os::convert_io_error(vm, e))?;

        fio_set_fileno(&instance, handle, vm)?;

        //return number of bytes written
        Ok(len)
    }

    #[cfg(windows)]
    fn file_io_close(instance: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        let raw_handle = i64::try_from_object(vm, vm.get_attribute(instance.clone(), "__fileno")?)?;
        unsafe {
            winapi::um::handleapi::CloseHandle(raw_handle as _);
        }
        vm.set_attr(&instance, "closefd", vm.new_bool(true))?;
        vm.set_attr(&instance, "closed", vm.new_bool(true))?;
        Ok(())
    }

    #[cfg(unix)]
    fn file_io_close(instance: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        let raw_fd = i64::try_from_object(vm, vm.get_attribute(instance.clone(), "__fileno")?)?;
        unsafe {
            libc::close(raw_fd as _);
        }
        vm.set_attr(&instance, "closefd", vm.new_bool(true))?;
        vm.set_attr(&instance, "closed", vm.new_bool(true))?;
        Ok(())
    }

    fn file_io_seekable(_self: PyObjectRef, _vm: &VirtualMachine) -> bool {
        true
    }

    fn file_io_fileno(instance: PyObjectRef, vm: &VirtualMachine) -> PyResult {
        vm.get_attribute(instance, "__fileno")
    }

    pub fn make_fileio(ctx: &crate::pyobject::PyContext, raw_io_base: PyClassRef) -> PyClassRef {
        py_class!(ctx, "FileIO", raw_io_base, {
            "__init__" => ctx.new_rustfunc(file_io_init),
            "name" => ctx.str_type(),
            "read" => ctx.new_rustfunc(file_io_read),
            "readinto" => ctx.new_rustfunc(file_io_readinto),
            "write" => ctx.new_rustfunc(file_io_write),
            "close" => ctx.new_rustfunc(file_io_close),
            "seekable" => ctx.new_rustfunc(file_io_seekable),
            "fileno" => ctx.new_rustfunc(file_io_fileno),
        })
    }
}

fn buffered_writer_write(instance: PyObjectRef, obj: PyObjectRef, vm: &VirtualMachine) -> PyResult {
    let raw = vm.get_attribute(instance, "raw").unwrap();

    //This should be replaced with a more appropriate chunking implementation
    vm.call_method(&raw, "write", vec![obj.clone()])
}

fn buffered_writer_seekable(_self: PyObjectRef, _vm: &VirtualMachine) -> bool {
    true
}

fn text_io_wrapper_init(
    instance: PyObjectRef,
    buffer: PyObjectRef,
    vm: &VirtualMachine,
) -> PyResult<()> {
    vm.set_attr(&instance, "buffer", buffer.clone())?;
    Ok(())
}

fn text_io_wrapper_seekable(_self: PyObjectRef, _vm: &VirtualMachine) -> bool {
    true
}

fn text_io_wrapper_read(
    instance: PyObjectRef,
    size: OptionalOption<PyObjectRef>,
    vm: &VirtualMachine,
) -> PyResult<String> {
    let buffered_reader_class = vm.try_class("_io", "BufferedReader")?;
    let raw = vm.get_attribute(instance.clone(), "buffer").unwrap();

    if !objtype::isinstance(&raw, &buffered_reader_class) {
        // TODO: this should be io.UnsupportedOperation error which derives both from ValueError *and* OSError
        return Err(vm.new_value_error("not readable".to_string()));
    }

    let bytes = vm.call_method(
        &raw,
        "read",
        vec![size.flat_option().unwrap_or_else(|| vm.get_none())],
    )?;
    let bytes = PyBytesLike::try_from_object(vm, bytes)?;
    //format bytes into string
    let rust_string = String::from_utf8(bytes.to_cow().into_owned()).map_err(|e| {
        vm.new_unicode_decode_error(format!(
            "cannot decode byte at index: {}",
            e.utf8_error().valid_up_to()
        ))
    })?;
    Ok(rust_string)
}

fn text_io_wrapper_write(
    instance: PyObjectRef,
    obj: PyStringRef,
    vm: &VirtualMachine,
) -> PyResult<usize> {
    use std::str::from_utf8;

    let buffered_writer_class = vm.try_class("_io", "BufferedWriter")?;
    let raw = vm.get_attribute(instance.clone(), "buffer").unwrap();

    if !objtype::isinstance(&raw, &buffered_writer_class) {
        // TODO: this should be io.UnsupportedOperation error which derives from ValueError and OSError
        return Err(vm.new_value_error("not writable".to_string()));
    }

    let bytes = obj.as_str().to_string().into_bytes();

    let len = vm.call_method(&raw, "write", vec![vm.ctx.new_bytes(bytes.clone())])?;
    let len = objint::get_value(&len).to_usize().ok_or_else(|| {
        vm.new_overflow_error("int to large to convert to Rust usize".to_string())
    })?;

    // returns the count of unicode code points written
    let len = from_utf8(&bytes[..len])
        .unwrap_or_else(|e| from_utf8(&bytes[..e.valid_up_to()]).unwrap())
        .chars()
        .count();
    Ok(len)
}

fn text_io_wrapper_readline(
    instance: PyObjectRef,
    size: OptionalOption<PyObjectRef>,
    vm: &VirtualMachine,
) -> PyResult<String> {
    let buffered_reader_class = vm.try_class("_io", "BufferedReader")?;
    let raw = vm.get_attribute(instance.clone(), "buffer").unwrap();

    if !objtype::isinstance(&raw, &buffered_reader_class) {
        // TODO: this should be io.UnsupportedOperation error which derives both from ValueError *and* OSError
        return Err(vm.new_value_error("not readable".to_string()));
    }

    let bytes = vm.call_method(
        &raw,
        "readline",
        vec![size.flat_option().unwrap_or_else(|| vm.get_none())],
    )?;
    let bytes = PyBytesLike::try_from_object(vm, bytes)?;
    //format bytes into string
    let rust_string = String::from_utf8(bytes.to_cow().into_owned()).map_err(|e| {
        vm.new_unicode_decode_error(format!(
            "cannot decode byte at index: {}",
            e.utf8_error().valid_up_to()
        ))
    })?;
    Ok(rust_string)
}

fn split_mode_string(mode_string: String) -> Result<(String, String), String> {
    let mut mode: char = '\0';
    let mut typ: char = '\0';
    let mut plus_is_set = false;

    for ch in mode_string.chars() {
        match ch {
            '+' => {
                if plus_is_set {
                    return Err(format!("invalid mode: '{}'", mode_string));
                }
                plus_is_set = true;
            }
            't' | 'b' => {
                if typ != '\0' {
                    if typ == ch {
                        // no duplicates allowed
                        return Err(format!("invalid mode: '{}'", mode_string));
                    } else {
                        return Err("can't have text and binary mode at once".to_string());
                    }
                }
                typ = ch;
            }
            'a' | 'r' | 'w' => {
                if mode != '\0' {
                    if mode == ch {
                        // no duplicates allowed
                        return Err(format!("invalid mode: '{}'", mode_string));
                    } else {
                        return Err(
                            "must have exactly one of create/read/write/append mode".to_string()
                        );
                    }
                }
                mode = ch;
            }
            _ => return Err(format!("invalid mode: '{}'", mode_string)),
        }
    }

    if mode == '\0' {
        return Err(
            "Must have exactly one of create/read/write/append mode and at most one plus"
                .to_string(),
        );
    }
    let mut mode = mode.to_string();
    if plus_is_set {
        mode.push('+');
    }
    if typ == '\0' {
        typ = 't';
    }
    Ok((mode, typ.to_string()))
}

pub fn io_open(vm: &VirtualMachine, args: PyFuncArgs) -> PyResult {
    arg_check!(
        vm,
        args,
        required = [(file, None)],
        optional = [(mode, Some(vm.ctx.str_type()))]
    );

    // mode is optional: 'rt' is the default mode (open from reading text)
    let mode_string = mode.map_or("rt".to_string(), objstr::get_value);

    let (mode, typ) = match split_mode_string(mode_string) {
        Ok((mode, typ)) => (mode, typ),
        Err(error_message) => {
            return Err(vm.new_value_error(error_message));
        }
    };

    let io_module = vm.import("_io", &[], 0)?;

    // Construct a FileIO (subclass of RawIOBase)
    // This is subsequently consumed by a Buffered Class.
    let file_io_class = vm.get_attribute(io_module.clone(), "FileIO").map_err(|_| {
        // TODO: UnsupportedOperation here
        vm.new_os_error(
            "Couldn't get FileIO, io.open likely isn't supported on your platform".to_string(),
        )
    })?;
    let file_io_obj = vm.invoke(
        &file_io_class,
        vec![file.clone(), vm.ctx.new_str(mode.clone())],
    )?;

    // Create Buffered class to consume FileIO. The type of buffered class depends on
    // the operation in the mode.
    // There are 3 possible classes here, each inheriting from the RawBaseIO
    // creating || writing || appending => BufferedWriter
    let buffered = match mode.chars().next().unwrap() {
        'w' => {
            let buffered_writer_class = vm
                .get_attribute(io_module.clone(), "BufferedWriter")
                .unwrap();
            vm.invoke(&buffered_writer_class, vec![file_io_obj.clone()])
        }
        'r' => {
            let buffered_reader_class = vm
                .get_attribute(io_module.clone(), "BufferedReader")
                .unwrap();
            vm.invoke(&buffered_reader_class, vec![file_io_obj.clone()])
        }
        //TODO: updating => PyBufferedRandom
        _ => unimplemented!("'a' mode is not yet implemented"),
    };

    let io_obj = match typ.chars().next().unwrap() {
        // If the mode is text this buffer type is consumed on construction of
        // a TextIOWrapper which is subsequently returned.
        't' => {
            let text_io_wrapper_class = vm.get_attribute(io_module, "TextIOWrapper").unwrap();
            vm.invoke(&text_io_wrapper_class, vec![buffered.unwrap()])
        }
        // If the mode is binary this Buffered class is returned directly at
        // this point.
        // For Buffered class construct "raw" IO class e.g. FileIO and pass this into corresponding field
        'b' => buffered,
        _ => unreachable!(),
    };
    io_obj
}

pub fn make_module(vm: &VirtualMachine) -> PyObjectRef {
    let ctx = &vm.ctx;

    //IOBase the abstract base class of the IO Module
    let io_base = py_class!(ctx, "_IOBase", ctx.object(), {
        "__enter__" => ctx.new_rustfunc(io_base_cm_enter),
        "__exit__" => ctx.new_rustfunc(io_base_cm_exit),
        "seekable" => ctx.new_rustfunc(io_base_seekable),
        "readable" => ctx.new_rustfunc(io_base_readable),
        "writable" => ctx.new_rustfunc(io_base_writable),
        "flush" => ctx.new_rustfunc(io_base_flush),
        "closed" => ctx.new_property(io_base_closed),
        "__closed" => ctx.new_bool(false),
        "close" => ctx.new_rustfunc(io_base_close),
        "readline" => ctx.new_rustfunc(io_base_readline),
        "_checkClosed" => ctx.new_rustfunc(io_base_checkclosed),
        "_checkReadable" => ctx.new_rustfunc(io_base_checkreadable),
        "_checkWritable" => ctx.new_rustfunc(io_base_checkwritable),
        "_checkSeekable" => ctx.new_rustfunc(io_base_checkseekable),
        "__iter__" => ctx.new_rustfunc(io_base_iter),
        "__next__" => ctx.new_rustfunc(io_base_next),
        "readlines" => ctx.new_rustfunc(io_base_readlines),
    });

    // IOBase Subclasses
    let raw_io_base = py_class!(ctx, "_RawIOBase", io_base.clone(), {
        "read" => ctx.new_rustfunc(raw_io_base_read),
    });

    let buffered_io_base = py_class!(ctx, "_BufferedIOBase", io_base.clone(), {});

    //TextIO Base has no public constructor
    let text_io_base = py_class!(ctx, "_TextIOBase", io_base.clone(), {});

    // BufferedIOBase Subclasses
    let buffered_reader = py_class!(ctx, "BufferedReader", buffered_io_base.clone(), {
        //workaround till the buffered classes can be fixed up to be more
        //consistent with the python model
        //For more info see: https://github.com/RustPython/RustPython/issues/547
        "__init__" => ctx.new_rustfunc(buffered_io_base_init),
        "read" => ctx.new_rustfunc(buffered_reader_read),
        "seekable" => ctx.new_rustfunc(buffered_reader_seekable),
        "close" => ctx.new_rustfunc(buffered_reader_close),
        "fileno" => ctx.new_rustfunc(buffered_io_base_fileno),
    });

    let buffered_writer = py_class!(ctx, "BufferedWriter", buffered_io_base.clone(), {
        //workaround till the buffered classes can be fixed up to be more
        //consistent with the python model
        //For more info see: https://github.com/RustPython/RustPython/issues/547
        "__init__" => ctx.new_rustfunc(buffered_io_base_init),
        "write" => ctx.new_rustfunc(buffered_writer_write),
        "seekable" => ctx.new_rustfunc(buffered_writer_seekable),
        "fileno" => ctx.new_rustfunc(buffered_io_base_fileno),
    });

    //TextIOBase Subclass
    let text_io_wrapper = py_class!(ctx, "TextIOWrapper", text_io_base.clone(), {
        "__init__" => ctx.new_rustfunc(text_io_wrapper_init),
        "seekable" => ctx.new_rustfunc(text_io_wrapper_seekable),
        "read" => ctx.new_rustfunc(text_io_wrapper_read),
        "write" => ctx.new_rustfunc(text_io_wrapper_write),
        "readline" => ctx.new_rustfunc(text_io_wrapper_readline),
    });

    //StringIO: in-memory text
    let string_io = py_class!(ctx, "StringIO", text_io_base.clone(), {
        (slot new) => string_io_new,
        "seek" => ctx.new_rustfunc(PyStringIORef::seek),
        "seekable" => ctx.new_rustfunc(PyStringIORef::seekable),
        "read" => ctx.new_rustfunc(PyStringIORef::read),
        "write" => ctx.new_rustfunc(PyStringIORef::write),
        "getvalue" => ctx.new_rustfunc(PyStringIORef::getvalue),
        "tell" => ctx.new_rustfunc(PyStringIORef::tell),
        "readline" => ctx.new_rustfunc(PyStringIORef::readline),
        "truncate" => ctx.new_rustfunc(PyStringIORef::truncate),
        "closed" => ctx.new_property(PyStringIORef::closed),
        "close" => ctx.new_rustfunc(PyStringIORef::close),
    });

    //BytesIO: in-memory bytes
    let bytes_io = py_class!(ctx, "BytesIO", buffered_io_base.clone(), {
        (slot new) => bytes_io_new,
        "read" => ctx.new_rustfunc(PyBytesIORef::read),
        "read1" => ctx.new_rustfunc(PyBytesIORef::read),
        "seek" => ctx.new_rustfunc(PyBytesIORef::seek),
        "seekable" => ctx.new_rustfunc(PyBytesIORef::seekable),
        "write" => ctx.new_rustfunc(PyBytesIORef::write),
        "getvalue" => ctx.new_rustfunc(PyBytesIORef::getvalue),
        "tell" => ctx.new_rustfunc(PyBytesIORef::tell),
        "readline" => ctx.new_rustfunc(PyBytesIORef::readline),
        "truncate" => ctx.new_rustfunc(PyBytesIORef::truncate),
        "closed" => ctx.new_property(PyBytesIORef::closed),
        "close" => ctx.new_rustfunc(PyBytesIORef::close),
    });

    let module = py_module!(vm, "_io", {
        "open" => ctx.new_rustfunc(io_open),
        "_IOBase" => io_base,
        "_RawIOBase" => raw_io_base.clone(),
        "_BufferedIOBase" => buffered_io_base,
        "_TextIOBase" => text_io_base,
        "BufferedReader" => buffered_reader,
        "BufferedWriter" => buffered_writer,
        "TextIOWrapper" => text_io_wrapper,
        "StringIO" => string_io,
        "BytesIO" => bytes_io,
        "DEFAULT_BUFFER_SIZE" => ctx.new_int(8 * 1024),
    });

    #[cfg(not(target_arch = "wasm32"))]
    extend_module!(vm, module, {
        "FileIO" => fileio::make_fileio(ctx, raw_io_base),
    });

    module
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_mode_split_into(mode_string: &str, expected_mode: &str, expected_typ: &str) {
        let (mode, typ) = split_mode_string(mode_string.to_string()).unwrap();
        assert_eq!(mode, expected_mode);
        assert_eq!(typ, expected_typ);
    }

    #[test]
    fn test_split_mode_valid_cases() {
        assert_mode_split_into("r", "r", "t");
        assert_mode_split_into("rb", "r", "b");
        assert_mode_split_into("rt", "r", "t");
        assert_mode_split_into("r+t", "r+", "t");
        assert_mode_split_into("w+t", "w+", "t");
        assert_mode_split_into("r+b", "r+", "b");
        assert_mode_split_into("w+b", "w+", "b");
    }

    #[test]
    fn test_invalid_mode() {
        assert_eq!(
            split_mode_string("rbsss".to_string()),
            Err("invalid mode: 'rbsss'".to_string())
        );
        assert_eq!(
            split_mode_string("rrb".to_string()),
            Err("invalid mode: 'rrb'".to_string())
        );
        assert_eq!(
            split_mode_string("rbb".to_string()),
            Err("invalid mode: 'rbb'".to_string())
        );
    }

    #[test]
    fn test_mode_not_specified() {
        assert_eq!(
            split_mode_string("".to_string()),
            Err(
                "Must have exactly one of create/read/write/append mode and at most one plus"
                    .to_string()
            )
        );
        assert_eq!(
            split_mode_string("b".to_string()),
            Err(
                "Must have exactly one of create/read/write/append mode and at most one plus"
                    .to_string()
            )
        );
        assert_eq!(
            split_mode_string("t".to_string()),
            Err(
                "Must have exactly one of create/read/write/append mode and at most one plus"
                    .to_string()
            )
        );
    }

    #[test]
    fn test_text_and_binary_at_once() {
        assert_eq!(
            split_mode_string("rbt".to_string()),
            Err("can't have text and binary mode at once".to_string())
        );
    }

    #[test]
    fn test_exactly_one_mode() {
        assert_eq!(
            split_mode_string("rwb".to_string()),
            Err("must have exactly one of create/read/write/append mode".to_string())
        );
    }

    #[test]
    fn test_at_most_one_plus() {
        assert_eq!(
            split_mode_string("a++".to_string()),
            Err("invalid mode: 'a++'".to_string())
        );
    }

    #[test]
    fn test_buffered_read() {
        let data = vec![1, 2, 3, 4];
        let bytes: i64 = -1;
        let mut buffered = BufferedIO {
            cursor: Cursor::new(data.clone()),
        };

        assert_eq!(buffered.read(bytes).unwrap(), data);
    }

    #[test]
    fn test_buffered_seek() {
        let data = vec![1, 2, 3, 4];
        let count: u64 = 2;
        let mut buffered = BufferedIO {
            cursor: Cursor::new(data.clone()),
        };

        assert_eq!(buffered.seek(count.clone()).unwrap(), count);
        assert_eq!(buffered.read(count.clone() as i64).unwrap(), vec![3, 4]);
    }

    #[test]
    fn test_buffered_value() {
        let data = vec![1, 2, 3, 4];
        let buffered = BufferedIO {
            cursor: Cursor::new(data.clone()),
        };

        assert_eq!(buffered.getvalue(), data);
    }
}
