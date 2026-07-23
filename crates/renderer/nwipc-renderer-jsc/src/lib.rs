//! `JavaScriptCore` adapter for the renderer core.
//!
//! `WebKit` lifecycle code owns the context and must call [`JscBinding::teardown`] before the
//! context is destroyed. This crate does not import any `WebKit` API.

use std::ffi::c_void;
use std::thread::{self, ThreadId};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_renderer_api::RendererTransport;
#[cfg(target_os = "macos")]
use nwipc_renderer_api::TransportEvent;
#[cfg(target_os = "macos")]
use nwipc_renderer_core::RendererCore;
use nwipc_types::DocumentGeneration;

/// Factory invoked by `globalThis.__nwipc.connect()`.
pub trait TransportFactory: 'static {
    /// Creates one native transport for the active document.
    ///
    /// # Errors
    ///
    /// Returns a bootstrap or provider error when a port cannot be connected.
    fn connect(&mut self) -> Result<Box<dyn RendererTransport>, ErrorReport>;
}

impl<Factory> TransportFactory for Factory
where
    Factory: FnMut() -> Result<Box<dyn RendererTransport>, ErrorReport> + 'static,
{
    fn connect(&mut self) -> Result<Box<dyn RendererTransport>, ErrorReport> {
        self()
    }
}

/// Opaque `JavaScriptCore` context borrowed from the embedding runtime.
#[derive(Clone, Copy)]
pub struct JscContext(*const c_void);

impl JscContext {
    /// Wraps a live `JSContextRef`.
    ///
    /// # Safety
    ///
    /// The pointer must be a valid `JavaScriptCore` context and outlive the installed binding.
    pub const unsafe fn from_raw(context: *const c_void) -> Self {
        Self(context)
    }

    /// Returns the borrowed raw `JSContextRef`.
    pub const fn as_raw(self) -> *const c_void {
        self.0
    }
}

/// Installed, document-scoped `JavaScriptCore` binding.
pub struct JscBinding {
    context: JscContext,
    generation: DocumentGeneration,
    owner_thread: ThreadId,
    installed: bool,
}

impl JscBinding {
    /// Installs a frozen `globalThis.__nwipc` object in a live JSC context.
    ///
    /// # Errors
    ///
    /// Returns a typed platform or lifecycle error when installation fails.
    pub fn install(
        context: JscContext,
        generation: DocumentGeneration,
        factory: impl TransportFactory,
    ) -> Result<Self, ErrorReport> {
        if context.as_raw().is_null() {
            return Err(jsc_error(ErrorCode::InvalidRange, "JSC context"));
        }
        platform::install(context, generation, Box::new(factory))?;
        Ok(Self {
            context,
            generation,
            owner_thread: thread::current().id(),
            installed: true,
        })
    }

    /// Polls native transports and invokes protected JavaScript handlers in FIFO order.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle error off the installation thread or after teardown.
    pub fn dispatch(&mut self) -> Result<(), ErrorReport> {
        self.require_active()?;
        platform::dispatch(self.context)
    }

    /// Invalidates the document and releases every protected callback.
    ///
    /// This must run before the embedding runtime releases the JSC context.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle error when called from another thread.
    pub fn teardown(&mut self) -> Result<(), ErrorReport> {
        if thread::current().id() != self.owner_thread {
            return Err(jsc_error(
                ErrorCode::InvalidStateTransition,
                "JSC thread affinity",
            ));
        }
        if self.installed {
            platform::teardown(self.context, self.generation)?;
            self.installed = false;
        }
        Ok(())
    }

    fn require_active(&self) -> Result<(), ErrorReport> {
        if thread::current().id() != self.owner_thread {
            return Err(jsc_error(
                ErrorCode::InvalidStateTransition,
                "JSC thread affinity",
            ));
        }
        if !self.installed {
            return Err(jsc_error(ErrorCode::Closed, "JSC binding teardown"));
        }
        Ok(())
    }
}

impl Drop for JscBinding {
    fn drop(&mut self) {
        if self.installed && thread::current().id() == self.owner_thread {
            platform::abandon(self.context);
        }
    }
}

fn jsc_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Platform,
        code,
        if code == ErrorCode::Closed {
            Recoverability::Terminal
        } else {
            Recoverability::ReplaceEndpoint
        },
        operation,
    )
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::{CString, c_char};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::ptr;
    use std::rc::Rc;

    use nwipc_renderer_api::{DocumentPort, RendererContext, RendererRuntime, SendDisposition};

    use super::{
        DocumentGeneration, ErrorCode, ErrorReport, JscContext, RendererCore, TransportEvent,
        TransportFactory, c_void, jsc_error,
    };

    type JSContextRef = *const c_void;
    type JSObjectRef = *const c_void;
    type JSValueRef = *const c_void;
    type JSStringRef = *const c_void;
    type JSValueRefPtr = *mut JSValueRef;
    type JSCallback = unsafe extern "C" fn(
        JSContextRef,
        JSObjectRef,
        JSObjectRef,
        usize,
        *const JSValueRef,
        JSValueRefPtr,
    ) -> JSValueRef;

    const READ_ONLY: u32 = 1 << 1;
    const DONT_DELETE: u32 = 1 << 3;
    const UINT8_ARRAY: i32 = 3;

    #[link(name = "JavaScriptCore", kind = "framework")]
    unsafe extern "C" {
        fn JSContextGetGlobalObject(context: JSContextRef) -> JSObjectRef;
        fn JSStringCreateWithUTF8CString(string: *const c_char) -> JSStringRef;
        fn JSStringRelease(string: JSStringRef);
        fn JSObjectMake(
            context: JSContextRef,
            class: *const c_void,
            data: *mut c_void,
        ) -> JSObjectRef;
        fn JSObjectMakeFunctionWithCallback(
            context: JSContextRef,
            name: JSStringRef,
            callback: JSCallback,
        ) -> JSObjectRef;
        fn JSObjectSetProperty(
            context: JSContextRef,
            object: JSObjectRef,
            name: JSStringRef,
            value: JSValueRef,
            attributes: u32,
            exception: JSValueRefPtr,
        );
        fn JSObjectGetProperty(
            context: JSContextRef,
            object: JSObjectRef,
            name: JSStringRef,
            exception: JSValueRefPtr,
        ) -> JSValueRef;
        fn JSObjectCallAsFunction(
            context: JSContextRef,
            object: JSObjectRef,
            this_object: JSObjectRef,
            argument_count: usize,
            arguments: *const JSValueRef,
            exception: JSValueRefPtr,
        ) -> JSValueRef;
        fn JSValueProtect(context: JSContextRef, value: JSValueRef);
        fn JSValueUnprotect(context: JSContextRef, value: JSValueRef);
        fn JSObjectMakeTypedArray(
            context: JSContextRef,
            array_type: i32,
            length: usize,
            exception: JSValueRefPtr,
        ) -> JSObjectRef;
        fn JSValueGetTypedArrayType(
            context: JSContextRef,
            value: JSValueRef,
            exception: JSValueRefPtr,
        ) -> i32;
        fn JSObjectGetTypedArrayBytesPtr(
            context: JSContextRef,
            object: JSObjectRef,
            exception: JSValueRefPtr,
        ) -> *mut c_void;
        fn JSObjectGetTypedArrayByteLength(
            context: JSContextRef,
            object: JSObjectRef,
            exception: JSValueRefPtr,
        ) -> usize;
        fn JSValueIsUndefined(context: JSContextRef, value: JSValueRef) -> bool;
        fn JSValueIsObject(context: JSContextRef, value: JSValueRef) -> bool;
        fn JSValueToObject(
            context: JSContextRef,
            value: JSValueRef,
            exception: JSValueRefPtr,
        ) -> JSObjectRef;
        fn JSValueMakeUndefined(context: JSContextRef) -> JSValueRef;
        fn JSValueMakeNumber(context: JSContextRef, number: f64) -> JSValueRef;
        fn JSValueToNumber(
            context: JSContextRef,
            value: JSValueRef,
            exception: JSValueRefPtr,
        ) -> f64;
        fn JSValueMakeString(context: JSContextRef, string: JSStringRef) -> JSValueRef;
        fn JSEvaluateScript(
            context: JSContextRef,
            script: JSStringRef,
            this_object: JSObjectRef,
            source_url: JSStringRef,
            starting_line_number: i32,
            exception: JSValueRefPtr,
        ) -> JSValueRef;
    }

    struct BindingState {
        core: RendererCore,
        generation: DocumentGeneration,
        factory: Box<dyn TransportFactory>,
        handlers: HashMap<DocumentPort, JSObjectRef>,
        ports: HashMap<DocumentPort, JSObjectRef>,
    }

    thread_local! {
        static BINDINGS: RefCell<HashMap<usize, Rc<RefCell<BindingState>>>> = RefCell::new(HashMap::new());
    }

    pub(super) fn install(
        context: JscContext,
        generation: DocumentGeneration,
        factory: Box<dyn TransportFactory>,
    ) -> Result<(), ErrorReport> {
        let key = context.as_raw() as usize;
        let mut core = RendererCore::new();
        core.install_binding(RendererContext { generation })?;
        if BINDINGS.with_borrow(|bindings| bindings.contains_key(&key)) {
            return Err(jsc_error(
                ErrorCode::InvalidStateTransition,
                "duplicate JSC binding",
            ));
        }
        BINDINGS.with_borrow_mut(|bindings| {
            bindings.insert(
                key,
                Rc::new(RefCell::new(BindingState {
                    core,
                    generation,
                    factory,
                    handlers: HashMap::new(),
                    ports: HashMap::new(),
                })),
            );
        });
        let result = install_global(context.as_raw());
        if result.is_err() {
            BINDINGS.with_borrow_mut(|bindings| {
                bindings.remove(&key);
            });
        }
        result
    }

    pub(super) fn dispatch(context: JscContext) -> Result<(), ErrorReport> {
        let state = state(context.as_raw())?;
        state.borrow_mut().core.dispatch_readable()?;
        loop {
            let event = state.borrow_mut().core.pop_event();
            let Some(event) = event else { break };
            if matches!(event.event, TransportEvent::Writable) {
                if let Some(port_object) = state.borrow().ports.get(&event.port).copied() {
                    unsafe {
                        set_property(
                            context.as_raw(),
                            port_object,
                            "bufferedAmount",
                            JSValueMakeNumber(context.as_raw(), 0.0),
                            0,
                        )?;
                    }
                }
            }
            let handler = state.borrow().handlers.get(&event.port).copied();
            let dispatch_result = handler.map_or(Ok(()), |handler| {
                dispatch_event(context.as_raw(), handler, &event.event)
            });
            let terminal = matches!(
                event.event,
                TransportEvent::Closed | TransportEvent::Error(_)
            );
            if terminal {
                let removed = state.borrow_mut().handlers.remove(&event.port);
                if let Some(value) = removed {
                    unsafe { JSValueUnprotect(context.as_raw(), value) };
                }
                if let Some(value) = state.borrow_mut().ports.remove(&event.port) {
                    unsafe { JSValueUnprotect(context.as_raw(), value) };
                }
            }
            dispatch_result?;
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn teardown(
        context: JscContext,
        generation: DocumentGeneration,
    ) -> Result<(), ErrorReport> {
        let state =
            BINDINGS.with_borrow_mut(|bindings| bindings.remove(&(context.as_raw() as usize)));
        let Some(state) = state else { return Ok(()) };
        let mut state = state.borrow_mut();
        if state.generation != generation {
            return Err(jsc_error(
                ErrorCode::StaleGeneration,
                "JSC document teardown",
            ));
        }
        state.core.invalidate_document(generation);
        for handler in state.handlers.drain().map(|(_, handler)| handler) {
            unsafe { JSValueUnprotect(context.as_raw(), handler) };
        }
        for port in state.ports.drain().map(|(_, port)| port) {
            unsafe { JSValueUnprotect(context.as_raw(), port) };
        }
        Ok(())
    }

    pub(super) fn abandon(context: JscContext) {
        let state =
            BINDINGS.with_borrow_mut(|bindings| bindings.remove(&(context.as_raw() as usize)));
        if let Some(state) = state {
            let mut state = state.borrow_mut();
            for value in state.handlers.drain().map(|(_, value)| value) {
                unsafe { JSValueUnprotect(context.as_raw(), value) };
            }
            for value in state.ports.drain().map(|(_, value)| value) {
                unsafe { JSValueUnprotect(context.as_raw(), value) };
            }
        }
    }

    fn state(context: JSContextRef) -> Result<Rc<RefCell<BindingState>>, ErrorReport> {
        BINDINGS
            .with_borrow(|bindings| bindings.get(&(context as usize)).cloned())
            .ok_or_else(|| jsc_error(ErrorCode::StaleGeneration, "JSC callback document"))
    }

    fn install_global(context: JSContextRef) -> Result<(), ErrorReport> {
        unsafe {
            let binding = JSObjectMake(context, ptr::null(), ptr::null_mut());
            set_function(context, binding, "connect", connect_callback)?;
            let global = JSContextGetGlobalObject(context);
            set_property(context, global, "__nwipc", binding, READ_ONLY | DONT_DELETE)?;
            evaluate(context, "Object.freeze(globalThis.__nwipc)")?;
        }
        Ok(())
    }

    unsafe fn make_port(
        context: JSContextRef,
        port: DocumentPort,
    ) -> Result<JSObjectRef, ErrorReport> {
        let object = unsafe { JSObjectMake(context, ptr::null(), ptr::null_mut()) };
        unsafe {
            set_property(
                context,
                object,
                "__nwipcPortId",
                JSValueMakeNumber(context, f64::from(port.port_id.get())),
                READ_ONLY | DONT_DELETE,
            )?;
            set_function(context, object, "send", send_callback)?;
            set_function(context, object, "close", close_callback)?;
            set_function(context, object, "setHandler", set_handler_callback)?;
            set_property(
                context,
                object,
                "bufferedAmount",
                JSValueMakeNumber(context, 0.0),
                0,
            )?;
        }
        Ok(object)
    }

    unsafe extern "C" fn connect_callback(
        context: JSContextRef,
        _function: JSObjectRef,
        _this_object: JSObjectRef,
        _argument_count: usize,
        _arguments: *const JSValueRef,
        exception: JSValueRefPtr,
    ) -> JSValueRef {
        ffi_boundary(context, exception, || {
            let state = state(context)?;
            let transport = state.borrow_mut().factory.connect()?;
            let generation = state.borrow().generation;
            let port = state.borrow_mut().core.connect(generation, transport)?;
            state.borrow_mut().core.register_callback(port)?;
            let object = unsafe { make_port(context, port)? };
            unsafe { JSValueProtect(context, object) };
            state.borrow_mut().ports.insert(port, object);
            Ok(object)
        })
    }

    unsafe extern "C" fn send_callback(
        context: JSContextRef,
        _function: JSObjectRef,
        this_object: JSObjectRef,
        argument_count: usize,
        arguments: *const JSValueRef,
        exception: JSValueRefPtr,
    ) -> JSValueRef {
        ffi_boundary(context, exception, || {
            if argument_count != 1 {
                return Err(jsc_error(
                    ErrorCode::InvalidRange,
                    "JSC send argument count",
                ));
            }
            let argument = unsafe { *arguments };
            let payload = unsafe { copy_uint8_array(context, argument)? };
            let state = state(context)?;
            let port = unsafe { port_handle(context, this_object, &state.borrow())? };
            let disposition = state.borrow_mut().core.send(port, &payload)?;
            let buffered = state.borrow().core.buffered_amount(port)?;
            unsafe {
                set_property(
                    context,
                    this_object,
                    "bufferedAmount",
                    JSValueMakeNumber(context, f64::from(buffered)),
                    0,
                )?;
            }
            let result = match disposition {
                SendDisposition::Sent => "sent",
                SendDisposition::Backpressured => "backpressured",
            };
            Ok(unsafe { js_string_value(context, result) })
        })
    }

    unsafe extern "C" fn close_callback(
        context: JSContextRef,
        _function: JSObjectRef,
        this_object: JSObjectRef,
        _argument_count: usize,
        _arguments: *const JSValueRef,
        exception: JSValueRefPtr,
    ) -> JSValueRef {
        ffi_boundary(context, exception, || {
            let state = state(context)?;
            let port = unsafe { port_handle(context, this_object, &state.borrow())? };
            state.borrow_mut().core.close(port)?;
            Ok(unsafe { JSValueMakeUndefined(context) })
        })
    }

    unsafe extern "C" fn set_handler_callback(
        context: JSContextRef,
        _function: JSObjectRef,
        this_object: JSObjectRef,
        argument_count: usize,
        arguments: *const JSValueRef,
        exception: JSValueRefPtr,
    ) -> JSValueRef {
        ffi_boundary(context, exception, || {
            if argument_count != 1 {
                return Err(jsc_error(
                    ErrorCode::InvalidRange,
                    "JSC handler argument count",
                ));
            }
            let value = unsafe { *arguments };
            let state = state(context)?;
            let port = unsafe { port_handle(context, this_object, &state.borrow())? };
            let previous = state.borrow_mut().handlers.remove(&port);
            if let Some(previous) = previous {
                unsafe { JSValueUnprotect(context, previous) };
            }
            if !unsafe { JSValueIsUndefined(context, value) } {
                if !unsafe { JSValueIsObject(context, value) } {
                    return Err(jsc_error(ErrorCode::ProtocolViolation, "JSC handler type"));
                }
                let handler = unsafe { JSValueToObject(context, value, exception) };
                if exception_set(exception) {
                    return Err(jsc_error(
                        ErrorCode::ProtocolViolation,
                        "JSC handler conversion",
                    ));
                }
                unsafe { JSValueProtect(context, handler) };
                state.borrow_mut().handlers.insert(port, handler);
            }
            Ok(unsafe { JSValueMakeUndefined(context) })
        })
    }

    fn ffi_boundary(
        context: JSContextRef,
        exception: JSValueRefPtr,
        operation: impl FnOnce() -> Result<JSValueRef, ErrorReport>,
    ) -> JSValueRef {
        match catch_unwind(AssertUnwindSafe(operation)) {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                set_exception(context, exception, &format!("NWIPC_{:?}", error.code()));
                unsafe { JSValueMakeUndefined(context) }
            }
            Err(_) => {
                set_exception(context, exception, "NWIPC_INTERNAL");
                unsafe { JSValueMakeUndefined(context) }
            }
        }
    }

    unsafe fn copy_uint8_array(
        context: JSContextRef,
        value: JSValueRef,
    ) -> Result<Vec<u8>, ErrorReport> {
        let mut exception = ptr::null();
        if unsafe { JSValueGetTypedArrayType(context, value, &mut exception) } != UINT8_ARRAY
            || !exception.is_null()
        {
            return Err(jsc_error(
                ErrorCode::ProtocolViolation,
                "JSC Uint8Array argument",
            ));
        }
        let object = unsafe { JSValueToObject(context, value, &mut exception) };
        let length = unsafe { JSObjectGetTypedArrayByteLength(context, object, &mut exception) };
        let bytes = unsafe { JSObjectGetTypedArrayBytesPtr(context, object, &mut exception) };
        if !exception.is_null() || (bytes.is_null() && length != 0) {
            return Err(jsc_error(
                ErrorCode::ProtocolViolation,
                "JSC Uint8Array bytes",
            ));
        }
        if length == 0 {
            return Ok(Vec::new());
        }
        Ok(unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), length) }.to_vec())
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    unsafe fn port_handle(
        context: JSContextRef,
        object: JSObjectRef,
        state: &BindingState,
    ) -> Result<DocumentPort, ErrorReport> {
        let value = unsafe { get_property(context, object, "__nwipcPortId")? };
        let mut exception = ptr::null();
        let number = unsafe { JSValueToNumber(context, value, &mut exception) };
        if !exception.is_null()
            || !number.is_finite()
            || number.fract() != 0.0
            || number < 1.0
            || number > f64::from(u32::MAX)
        {
            return Err(jsc_error(ErrorCode::ProtocolViolation, "JSC port receiver"));
        }
        let value = number as u32;
        let port_id = nwipc_types::PortId::new(value)
            .ok_or_else(|| jsc_error(ErrorCode::ProtocolViolation, "JSC port identity"))?;
        Ok(DocumentPort {
            generation: state.generation,
            port_id,
        })
    }

    fn dispatch_event(
        context: JSContextRef,
        handler: JSObjectRef,
        event: &TransportEvent,
    ) -> Result<(), ErrorReport> {
        let (method, argument) = unsafe {
            match event {
                TransportEvent::Message(payload) => {
                    let mut exception = ptr::null();
                    let array =
                        JSObjectMakeTypedArray(context, UINT8_ARRAY, payload.len(), &mut exception);
                    if !exception.is_null() || array.is_null() {
                        return Err(jsc_error(ErrorCode::Internal, "JSC receive allocation"));
                    }
                    let bytes = JSObjectGetTypedArrayBytesPtr(context, array, &mut exception);
                    if !exception.is_null() || (bytes.is_null() && !payload.is_empty()) {
                        return Err(jsc_error(ErrorCode::Internal, "JSC receive buffer"));
                    }
                    if !payload.is_empty() {
                        ptr::copy_nonoverlapping(
                            payload.as_ptr(),
                            bytes.cast::<u8>(),
                            payload.len(),
                        );
                    }
                    ("message", Some(array as JSValueRef))
                }
                TransportEvent::Writable => ("writable", None),
                TransportEvent::Closed => ("close", None),
                TransportEvent::Error(error) => (
                    "error",
                    Some(js_string_value(context, &format!("{:?}", error.code()))),
                ),
            }
        };
        unsafe { call_method(context, handler, method, argument) }
    }

    unsafe fn call_method(
        context: JSContextRef,
        object: JSObjectRef,
        method: &str,
        argument: Option<JSValueRef>,
    ) -> Result<(), ErrorReport> {
        let function_value = unsafe { get_property(context, object, method)? };
        if unsafe { JSValueIsUndefined(context, function_value) } {
            return Ok(());
        }
        let mut exception = ptr::null();
        let function = unsafe { JSValueToObject(context, function_value, &mut exception) };
        if !exception.is_null() {
            return Err(jsc_error(
                ErrorCode::ProtocolViolation,
                "JSC handler method",
            ));
        }
        let argument_storage = argument.unwrap_or(ptr::null());
        let (count, arguments) = if argument.is_some() {
            (1, &raw const argument_storage)
        } else {
            (0, ptr::null())
        };
        unsafe {
            JSObjectCallAsFunction(context, function, object, count, arguments, &mut exception)
        };
        if exception.is_null() {
            Ok(())
        } else {
            Err(jsc_error(ErrorCode::Internal, "JSC handler exception"))
        }
    }

    unsafe fn set_function(
        context: JSContextRef,
        object: JSObjectRef,
        name: &str,
        callback: JSCallback,
    ) -> Result<(), ErrorReport> {
        let name_ref = JsString::new(name);
        let function = unsafe { JSObjectMakeFunctionWithCallback(context, name_ref.0, callback) };
        unsafe { set_property(context, object, name, function, READ_ONLY | DONT_DELETE) }
    }

    unsafe fn set_property(
        context: JSContextRef,
        object: JSObjectRef,
        name: &str,
        value: JSValueRef,
        attributes: u32,
    ) -> Result<(), ErrorReport> {
        let name = JsString::new(name);
        let mut exception = ptr::null();
        unsafe { JSObjectSetProperty(context, object, name.0, value, attributes, &mut exception) };
        if exception.is_null() {
            Ok(())
        } else {
            Err(jsc_error(ErrorCode::Internal, "JSC set property"))
        }
    }

    unsafe fn get_property(
        context: JSContextRef,
        object: JSObjectRef,
        name: &str,
    ) -> Result<JSValueRef, ErrorReport> {
        let name = JsString::new(name);
        let mut exception = ptr::null();
        let value = unsafe { JSObjectGetProperty(context, object, name.0, &mut exception) };
        if exception.is_null() {
            Ok(value)
        } else {
            Err(jsc_error(ErrorCode::ProtocolViolation, "JSC get property"))
        }
    }

    unsafe fn evaluate(context: JSContextRef, script: &str) -> Result<JSValueRef, ErrorReport> {
        let script = JsString::new(script);
        let mut exception = ptr::null();
        let value = unsafe {
            JSEvaluateScript(
                context,
                script.0,
                ptr::null(),
                ptr::null(),
                1,
                &mut exception,
            )
        };
        if exception.is_null() {
            Ok(value)
        } else {
            Err(jsc_error(ErrorCode::Internal, "JSC evaluate"))
        }
    }

    unsafe fn js_string_value(context: JSContextRef, value: &str) -> JSValueRef {
        let value = JsString::new(value);
        unsafe { JSValueMakeString(context, value.0) }
    }

    fn set_exception(context: JSContextRef, exception: JSValueRefPtr, message: &str) {
        if exception.is_null() {
            return;
        }
        unsafe { *exception = js_string_value(context, message) };
    }

    fn exception_set(exception: JSValueRefPtr) -> bool {
        !exception.is_null() && !unsafe { *exception }.is_null()
    }

    struct JsString(JSStringRef);

    impl JsString {
        fn new(value: &str) -> Self {
            let value = CString::new(value).expect("static JSC names contain no NUL");
            Self(unsafe { JSStringCreateWithUTF8CString(value.as_ptr()) })
        }
    }

    impl Drop for JsString {
        fn drop(&mut self) {
            unsafe { JSStringRelease(self.0) };
        }
    }

    #[cfg(test)]
    mod tests {
        use std::collections::VecDeque;
        use std::sync::{Mutex, MutexGuard};

        use nwipc_error::ErrorReport;
        use nwipc_renderer_api::{RendererTransport, SendDisposition, TransportEvent};
        use nwipc_types::DocumentGeneration;

        use super::{JSContextRef, JSValueRef, c_void, evaluate, ptr, state};
        use crate::{JscBinding, JscContext};

        unsafe extern "C" {
            fn JSGlobalContextCreate(class: *const c_void) -> JSContextRef;
            fn JSGlobalContextRelease(context: JSContextRef);
            fn JSValueToBoolean(context: JSContextRef, value: JSValueRef) -> bool;
        }

        static JSC_TEST_LOCK: Mutex<()> = Mutex::new(());

        fn serial_test_guard() -> MutexGuard<'static, ()> {
            JSC_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        #[derive(Default)]
        struct Loopback {
            events: VecDeque<TransportEvent>,
            buffered: u32,
        }

        impl RendererTransport for Loopback {
            fn send(&mut self, payload: &[u8]) -> Result<SendDisposition, ErrorReport> {
                self.buffered = u32::try_from(payload.len()).unwrap();
                self.events
                    .push_back(TransportEvent::Message(payload.to_vec()));
                Ok(SendDisposition::Sent)
            }

            fn buffered_amount(&self) -> Result<u32, ErrorReport> {
                Ok(self.buffered)
            }
            fn poll(&mut self) -> Result<Option<TransportEvent>, ErrorReport> {
                Ok(self.events.pop_front())
            }
            fn close(&mut self) -> Result<(), ErrorReport> {
                self.events.push_back(TransportEvent::Closed);
                Ok(())
            }
        }

        struct Context(JSContextRef);

        impl Context {
            fn new() -> Self {
                Self(unsafe { JSGlobalContextCreate(ptr::null()) })
            }
            fn evaluate_bool(&self, script: &str) -> bool {
                let value = unsafe { evaluate(self.0, script).unwrap() };
                unsafe { JSValueToBoolean(self.0, value) }
            }
        }

        impl Drop for Context {
            fn drop(&mut self) {
                unsafe { JSGlobalContextRelease(self.0) }
            }
        }

        #[test]
        fn installs_frozen_binding_and_copies_uint8_arrays() {
            let _guard = serial_test_guard();
            let context = Context::new();
            let raw = unsafe { JscContext::from_raw(context.0) };
            let mut binding = JscBinding::install(raw, DocumentGeneration::new(1).unwrap(), || {
                Ok(Box::<Loopback>::default() as Box<dyn RendererTransport>)
            })
            .unwrap();
            assert!(context.evaluate_bool("Object.isFrozen(__nwipc)"));
            assert!(context.evaluate_bool("globalThis.p=__nwipc.connect(); globalThis.got=''; p.setHandler({message(v){got=[...v].join(',')}}); p.send(new Uint8Array([1,2,3])); true"));
            binding.dispatch().unwrap();
            assert!(context.evaluate_bool("got === '1,2,3'"));
            assert!(context.evaluate_bool("try { p.send('wrong'); false } catch (_) { true }"));
            binding.teardown().unwrap();
            assert!(binding.dispatch().is_err());
        }

        #[test]
        fn teardown_blocks_stale_callbacks_and_releases_handlers() {
            let _guard = serial_test_guard();
            let context = Context::new();
            let raw = unsafe { JscContext::from_raw(context.0) };
            let mut binding = JscBinding::install(raw, DocumentGeneration::new(2).unwrap(), || {
                Ok(Box::<Loopback>::default() as Box<dyn RendererTransport>)
            })
            .unwrap();
            context.evaluate_bool(
                "globalThis.stale=__nwipc.connect(); stale.setHandler({message(){}}); true",
            );
            binding.teardown().unwrap();
            assert!(
                context.evaluate_bool(
                    "try { stale.send(new Uint8Array()); false } catch (_) { true }"
                )
            );
        }

        #[test]
        fn repeated_document_lifecycle_releases_protected_objects() {
            let _guard = serial_test_guard();
            for generation in 3..67 {
                let context = Context::new();
                let raw = unsafe { JscContext::from_raw(context.0) };
                let mut binding =
                    JscBinding::install(raw, DocumentGeneration::new(generation).unwrap(), || {
                        Ok(Box::<Loopback>::default() as Box<dyn RendererTransport>)
                    })
                    .unwrap();
                assert!(context.evaluate_bool(
                    "for(let i=0;i<16;i++){const q=__nwipc.connect();q.setHandler({close(){}});q.close()} true"
                ));
                binding.dispatch().unwrap();
                let binding_state = state(context.0).unwrap();
                assert!(binding_state.borrow().handlers.is_empty());
                assert!(binding_state.borrow().ports.is_empty());
                binding.teardown().unwrap();
                assert!(state(context.0).is_err());
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub(super) fn install(
        _context: JscContext,
        _generation: DocumentGeneration,
        _factory: Box<dyn TransportFactory>,
    ) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported("JavaScriptCore macOS binding"))
    }

    pub(super) fn dispatch(_context: JscContext) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported("JavaScriptCore macOS binding"))
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn teardown(
        _context: JscContext,
        _generation: DocumentGeneration,
    ) -> Result<(), ErrorReport> {
        Ok(())
    }

    pub(super) fn abandon(_context: JscContext) {}
}
