use baseview::{Event, Size, Window, WindowEvent as BaseviewWindowEvent, WindowHandle, WindowInfo, WindowOpenOptions, WindowScalePolicy, gl::GlConfig};
use nih_plug::prelude::{Editor, GuiContext, ParamSetter};
use once_cell::unsync::OnceCell;
use slint::platform::femtovg_renderer::FemtoVGRenderer;
use slint::platform::WindowAdapter;
use slint::{LogicalPosition, PhysicalSize};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

pub use baseview::{DropData, DropEffect, EventStatus, MouseEvent};

// Re export slint so users can access it
pub use slint;

use serde::{Deserialize, Serialize};

type EventLoopHandler<T> = dyn Fn(&WindowHandler<T>, ParamSetter, &mut Window) + Send + Sync;

/// Persistent state for the Slint editor
///
/// This struct stores window size information that persists across plugin instances.
/// Use the `#[persist = "editor-state"]` attribute on a field of this type in your
/// plugin's parameter struct to enable automatic state persistence.
///
/// # Example
/// ```rust,ignore
/// #[derive(Params)]
/// struct MyParams {
///     #[persist = "editor-state"]
///     editor_state: Arc<SlintEditorState>,
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlintEditorState {
    /// Window width in physical pixels
    #[serde(default = "default_width")]
    pub width: u32,
    /// Window height in physical pixels
    #[serde(default = "default_height")]
    pub height: u32,
    /// Scale factor for the UI (1.0 = 100%)
    #[serde(default = "default_scale")]
    pub scale_factor: f32,
}

fn default_width() -> u32 { 400 }
fn default_height() -> u32 { 300 }
fn default_scale() -> f32 { 1.0 }

impl Default for SlintEditorState {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: default_height(),
            scale_factor: default_scale(),
        }
    }
}

impl SlintEditorState {
    /// Create a new editor state with the given size
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            scale_factor: 1.0,
        }
    }

    /// Create a new editor state with custom scale factor
    pub fn with_scale(width: u32, height: u32, scale_factor: f32) -> Self {
        Self {
            width,
            height,
            scale_factor,
        }
    }
}

/// A Slint based editor for NIH-plug
///
/// This struct manages the lifecycle of a Slint UI window within a NIH-plug plugin.
/// It handles window creation, event processing, and state persistence.
///
/// # State Persistence
///
/// To enable automatic window size persistence across plugin instances, store a
/// `SlintEditorState` in your plugin's parameter struct with the `#[persist]` attribute.
/// Then pass a reference to it when creating the editor.
///
/// # Example
///
/// ```rust,ignore
/// use nih_plug_slint::{SlintEditor, SlintEditorState};
/// use std::sync::Arc;
///
/// #[derive(Params)]
/// struct MyParams {
///     #[persist = "editor-state"]
///     editor_state: Arc<SlintEditorState>,
/// }
///
/// fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
///     let editor_state = self.params.editor_state.clone();
///
///     Some(Box::new(
///         SlintEditor::with_factory(
///             || gui::AppWindow::new(),
///             (400, 300)  // Default size
///         )
///         .with_state(editor_state)  // Enable state persistence
///         .with_event_loop(|handler, _setter, _window| {
///             // Update UI here
///         })
///     ))
/// }
/// ```
pub struct SlintEditor<T: slint::ComponentHandle> {
    /// Factory function to create the Slint component
    component_factory: Arc<dyn Fn() -> Result<T, slint::PlatformError> + Send + Sync>,
    /// Current window width (updated on resize)
    width: Arc<AtomicU32>,
    /// Current window height (updated on resize)
    height: Arc<AtomicU32>,
    /// Optional persistent state for window size
    state: Option<Arc<SlintEditorState>>,
    /// Event loop handler called on each frame
    event_loop_handler: Arc<EventLoopHandler<T>>,
}

impl<T: slint::ComponentHandle + 'static> SlintEditor<T> {
    /// Create a new Slint editor with a factory function
    ///
    /// This is useful when you need to create fresh instances of the component.
    /// The factory is called each time the window is opened.
    ///
    /// # Arguments
    /// * `factory` - Function that creates a new Slint component
    /// * `size` - Default window size (width, height) in physical pixels
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// SlintEditor::with_factory(
    ///     || gui::AppWindow::new(),
    ///     (400, 300)
    /// )
    /// ```
    pub fn with_factory<F>(factory: F, size: (u32, u32)) -> Self
    where
        F: Fn() -> Result<T, slint::PlatformError> + 'static + Send + Sync,
    {
        Self {
            component_factory: Arc::new(factory),
            width: Arc::new(AtomicU32::new(size.0)),
            height: Arc::new(AtomicU32::new(size.1)),
            state: None,
            event_loop_handler: Arc::new(|_, _, _| {}),
        }
    }

    /// Attach a persistent state to enable window size persistence
    ///
    /// When a state is attached, the editor will:
    /// - Load the saved window size when opening
    /// - Save the window size when resizing
    ///
    /// # Arguments
    /// * `state` - Arc to a `SlintEditorState` that should be stored in your
    ///   plugin's params struct with `#[persist = "editor-state"]`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// SlintEditor::with_factory(|| gui::AppWindow::new(), (400, 300))
    ///     .with_state(self.params.editor_state.clone())
    /// ```
    pub fn with_state(mut self, state: Arc<SlintEditorState>) -> Self {
        // Load size from state
        self.width = Arc::new(AtomicU32::new(state.width));
        self.height = Arc::new(AtomicU32::new(state.height));
        self.state = Some(state);
        self
    }

    /// Set a custom event loop handler
    ///
    /// This handler is called on every frame and allows you to:
    /// - Update UI based on parameter changes (Plugin -> UI)
    /// - Set up callbacks for UI events (UI -> Plugin)
    /// - Sync any other state between plugin and UI
    ///
    /// The handler receives:
    /// - `WindowHandler<T>`: Access to the Slint component and window
    /// - `ParamSetter`: For setting plugin parameters from the UI
    /// - `&mut Window`: The baseview window (rarely needed directly)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// .with_event_loop({
    ///     let params = self.params.clone();
    ///     move |window_handler, _setter, _window| {
    ///         let component = window_handler.component();
    ///
    ///         // Plugin -> UI: Update UI with parameter values
    ///         let threshold = params.threshold.unmodulated_normalized_value();
    ///         component.set_threshold_value(threshold);
    ///
    ///         // UI -> Plugin: Set up callbacks
    ///         let params_clone = params.clone();
    ///         let context = window_handler.context().clone();
    ///         component.on_threshold_changed(move |value| {
    ///             let setter = ParamSetter::new(&*context);
    ///             setter.set_parameter_normalized(&params_clone.threshold, value);
    ///         });
    ///     }
    /// })
    /// ```
    pub fn with_event_loop<F>(mut self, handler: F) -> Self
    where
        F: Fn(&WindowHandler<T>, ParamSetter, &mut baseview::Window) + 'static + Send + Sync,
    {
        self.event_loop_handler = Arc::new(handler);
        self
    }
}

/// OpenGL interface implementation for baseview
struct BaseviewOpenGLInterface;

unsafe impl slint::platform::femtovg_renderer::OpenGLInterface for BaseviewOpenGLInterface {
    fn ensure_current(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // baseview ensures the context is current before calling on_frame
        Ok(())
    }

    fn swap_buffers(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // baseview handles buffer swapping
        Ok(())
    }

    fn resize(
        &self,
        _width: core::num::NonZeroU32,
        _height: core::num::NonZeroU32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Resize is handled through the WindowAdapter
        Ok(())
    }

    fn get_proc_address(&self, name: &core::ffi::CStr) -> *const core::ffi::c_void {
        // Use glutin's GL loader to get OpenGL function pointers
        // This works across platforms (Windows/macOS/Linux)
        #[cfg(target_os = "windows")]
        {
            // On Windows, use wglGetProcAddress for extension functions
            // and GetProcAddress for core functions
            unsafe {
                // First check for OpenGL 1.1 core functions from opengl32.dll
                // These MUST come from the DLL, not wglGetProcAddress
                let opengl32 = LoadLibraryA(b"opengl32.dll\0".as_ptr());
                if !opengl32.is_null() {
                    let proc_addr = GetProcAddress(opengl32, name.as_ptr());
                    if !proc_addr.is_null() {
                        // Found in opengl32.dll (OpenGL 1.1 core function)
                        return proc_addr as *const core::ffi::c_void;
                    }
                }

                // Try wglGetProcAddress for extensions and OpenGL 1.2+ functions
                let addr = wglGetProcAddress(name.as_ptr());

                // CRITICAL: wglGetProcAddress can return invalid non-null pointers!
                // According to OpenGL Wiki and Windows docs, it can return 1, 2, 3, or -1
                // on failure instead of NULL. We must check for all these values.
                let addr_int = addr as usize;
                if addr_int != 0 && addr_int != 1 && addr_int != 2 && addr_int != 3 && addr_int != usize::MAX {
                    return addr as *const core::ffi::c_void;
                }
            }
        }

        // This hasnt been tested this is just my best guess
        // based on how dynamic loading works on macOS and Linux
        #[cfg(target_os = "macos")]
        {
            // On macOS, use dlsym to load OpenGL functions from the OpenGL framework
            unsafe {
                // RTLD_DEFAULT searches all loaded libraries
                const RTLD_DEFAULT: *mut core::ffi::c_void = -2isize as *mut core::ffi::c_void;
                let addr = dlsym(RTLD_DEFAULT, name.as_ptr());
                if !addr.is_null() {
                    return addr;
                }
            }
        }

        #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
        {
            // On Linux/other Unix, use dlsym to load OpenGL functions
            unsafe {
                const RTLD_DEFAULT: *mut core::ffi::c_void = 0 as *mut core::ffi::c_void;
                let addr = dlsym(RTLD_DEFAULT, name.as_ptr());
                if !addr.is_null() {
                    return addr;
                }
            }
        }

        std::ptr::null()
    }
}

// Platform-specific OpenGL function loading

// Windows-specific functions
#[cfg(target_os = "windows")]
extern "system" {
    fn wglGetProcAddress(name: *const i8) -> *const core::ffi::c_void;
    fn GetProcAddress(module: *mut core::ffi::c_void, name: *const i8) -> *const core::ffi::c_void;
    fn LoadLibraryA(name: *const u8) -> *mut core::ffi::c_void;
}

// macOS and Linux use dlsym from libdl
#[cfg(any(target_os = "macos", target_os = "linux"))]
extern "C" {
    fn dlsym(handle: *mut core::ffi::c_void, symbol: *const i8) -> *mut core::ffi::c_void;
}

// Thread-local storage for the current adapter
// This allows us to update the adapter when reopening the window
thread_local! {
    static CURRENT_ADAPTER: RefCell<Option<Rc<BaseviewSlintAdapter>>> = RefCell::new(None);
    // Track whether GL context is current - prevents renderer creation before on_frame
    static GL_CONTEXT_READY: RefCell<bool> = RefCell::new(false);
}

/// Platform implementation for Slint
struct BaseviewSlintPlatform;

impl slint::platform::Platform for BaseviewSlintPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        CURRENT_ADAPTER.with(|adapter| {
            adapter.borrow()
                .clone()
                .map(|a| a as Rc<dyn WindowAdapter>)
                .ok_or_else(|| slint::PlatformError::Other("No adapter set".into()))
        })
    }
}

/// Custom WindowAdapter that bridges baseview and Slint
struct BaseviewSlintAdapter {
    window: slint::Window,
    renderer: OnceCell<FemtoVGRenderer>,
    /// Physical size in actual pixels (for the OpenGL framebuffer)
    physical_size: RefCell<PhysicalSize>,
    /// Scale factor (e.g., 2.0 on Retina displays)
    scale_factor: RefCell<f32>,
}

impl BaseviewSlintAdapter {
    fn new(physical_width: u32, physical_height: u32, scale_factor: f32) -> Rc<Self> {
        Rc::new_cyclic(|weak_self| {
            let window = slint::Window::new(weak_self.clone() as _);
            // Don't create the renderer yet - wait until GL context is active
            // This will be lazily initialized on first render call

            Self {
                window,
                renderer: OnceCell::new(),
                physical_size: RefCell::new(PhysicalSize::new(physical_width, physical_height)),
                scale_factor: RefCell::new(scale_factor),
            }
        })
    }

    /// Update the size and scale factor (called when window is resized or scale changes)
    fn update_size(&self, physical_width: u32, physical_height: u32, scale_factor: f32) {
        *self.physical_size.borrow_mut() = PhysicalSize::new(physical_width, physical_height);
        *self.scale_factor.borrow_mut() = scale_factor;
    }
}

impl WindowAdapter for BaseviewSlintAdapter {
    fn window(&self) -> &slint::Window {
        &self.window
    }

    fn size(&self) -> PhysicalSize {
        *self.physical_size.borrow()
    }

    fn renderer(&self) -> &dyn slint::platform::Renderer {
        // Lazy initialization: create renderer on first access when GL context is active
        self.renderer.get_or_init(|| {
            // Check if GL context is ready
            let context_ready = GL_CONTEXT_READY.with(|ready| *ready.borrow());
            if !context_ready {
                panic!("CRITICAL: Slint tried to create renderer before GL context was ready! \
                       This means Slint 1.14.1 is calling renderer() during component creation, \
                       not during rendering. This is incompatible with baseview integration.");
            }

            FemtoVGRenderer::new(BaseviewOpenGLInterface)
                .expect("Failed to create FemtoVG renderer")
        })
    }

    fn request_redraw(&self) {
        // baseview handles redraws in on_frame
    }
}

/// Handler for the baseview window containing the Slint UI
///
/// This struct manages the per-window state and event handling for a Slint UI instance.
/// It's created when the window is opened and destroyed when it's closed.
pub struct WindowHandler<T: slint::ComponentHandle> {
    /// NIH-plug GUI context for parameter operations and host communication
    context: Arc<dyn GuiContext>,
    /// User-provided event loop handler called on each frame
    event_loop_handler: Arc<EventLoopHandler<T>>,
    /// Current window width in logical pixels (updated on resize)
    pub width: Arc<AtomicU32>,
    /// Current window height in logical pixels (updated on resize)
    pub height: Arc<AtomicU32>,
    /// Current scale factor (e.g., 2.0 on Retina displays)
    scale_factor: RefCell<f32>,
    /// Optional persistent state for saving window size
    state: Option<Arc<SlintEditorState>>,
    /// GUI-thread only resize queue - uses RefCell instead of Mutex since it never crosses threads
    /// Wrapped in Rc to allow sharing with callbacks
    pending_resizes: Rc<RefCell<Vec<(u32, u32)>>>,
    /// Track last cursor position for button events
    last_cursor_pos: RefCell<LogicalPosition>,
    /// Track if window has been shown (deferred until first frame when GL context is active)
    window_shown: RefCell<bool>,
    /// Slint component instance
    component: T,
    /// Window adapter that bridges baseview and Slint
    adapter: Rc<BaseviewSlintAdapter>,
}

impl<T: slint::ComponentHandle> WindowHandler<T> {
    /// Resize the window and optionally persist the new size
    ///
    /// This method:
    /// 1. Updates the internal width/height atomics (logical size)
    /// 2. Updates the Slint adapter's physical size
    /// 3. Notifies Slint window of new size
    /// 4. Notifies the host that resize is requested
    /// 5. Actually resizes the baseview window
    /// 6. If state persistence is enabled, saves the new size
    ///
    /// Note: width and height are in LOGICAL pixels (what the user sees).
    /// Physical pixels = logical * scale_factor
    pub fn resize(&self, window: &mut baseview::Window, width: u32, height: u32) {
        let scale = *self.scale_factor.borrow();
        let physical_width = (width as f32 * scale) as u32;
        let physical_height = (height as f32 * scale) as u32;

        self.width.store(width, Ordering::Relaxed);
        self.height.store(height, Ordering::Relaxed);

        // Update adapter with physical size and scale factor
        self.adapter.update_size(physical_width, physical_height, scale);

        // Notify Slint window of new size to trigger re-layout
        // Slint expects logical size here
        let slint_window = self.window();
        slint_window.dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(width as f32, height as f32),
        });

        // Request redraw to show changes immediately
        slint_window.request_redraw();

        // Notify host
        self.context.request_resize();

        // Resize baseview window (uses logical size, baseview handles physical conversion)
        window.resize(Size {
            width: width as f64,
            height: height as f64,
        });

        // Persist the new size if state is available
        if let Some(state) = &self.state {
            // Safe to use unsafe here because SlintEditorState only contains primitive types
            // and we need to update the Arc without mut reference
            let state_ptr = Arc::as_ptr(state) as *mut SlintEditorState;
            unsafe {
                (*state_ptr).width = width;
                (*state_ptr).height = height;
            }
        }
    }

    /// Handle a window info update from baseview (scale factor or size change)
    fn handle_window_info(&self, info: &WindowInfo) {
        let scale = info.scale() as f32;
        let physical_size = info.physical_size();

        *self.scale_factor.borrow_mut() = scale;

        // Update adapter with physical size
        self.adapter.update_size(physical_size.width, physical_size.height, scale);

        // Update our logical size tracking
        let logical_size = info.logical_size();
        self.width.store(logical_size.width as u32, Ordering::Relaxed);
        self.height.store(logical_size.height as u32, Ordering::Relaxed);

        // Notify Slint of the new size (logical)
        self.adapter.window.dispatch_event(slint::platform::WindowEvent::Resized {
            size: slint::LogicalSize::new(logical_size.width as f32, logical_size.height as f32),
        });

        // Also set the scale factor on the Slint window
        self.adapter.window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        });
    }

    /// Queue a resize request to be processed later
    /// This allows deferring resize operations to avoid borrow checker conflicts
    pub fn queue_resize(&self, width: u32, height: u32) {
        self.pending_resizes.borrow_mut().push((width, height));
    }

    /// Get access to the pending resizes queue for direct manipulation
    /// This is useful when you need to queue resizes from callbacks
    pub fn pending_resizes(&self) -> &Rc<RefCell<Vec<(u32, u32)>>> {
        &self.pending_resizes
    }

    /// Process any pending resize requests
    /// Returns the size that was applied, if any
    pub fn process_pending_resizes(&self, window: &mut baseview::Window) -> Option<(u32, u32)> {
        let mut queue = self.pending_resizes.borrow_mut();
        if let Some((width, height)) = queue.pop() {
            // Only process the most recent resize request to avoid lag
            queue.clear();
            drop(queue); // Release the borrow before calling resize

            self.resize(window, width, height);
            Some((width, height))
        } else {
            None
        }
    }

    /// Get reference to the Slint component
    pub fn component(&self) -> &T {
        &self.component
    }

    /// Get reference to the Slint window
    pub fn window(&self) -> &slint::Window {
        &self.adapter.window
    }

    /// Get reference to the GUI context for parameter operations
    pub fn context(&self) -> &Arc<dyn GuiContext> {
        &self.context
    }

    /// Helper to set a parameter value from the UI
    ///
    /// # Arguments
    /// * `param` - The parameter to set
    /// * `normalized` - The normalized value (0.0 to 1.0)
    pub fn set_parameter_normalized(&self, param: &impl nih_plug::prelude::Param, normalized: f32) {
        let setter = ParamSetter::new(&*self.context);
        setter.set_parameter_normalized(param, normalized);
    }

    /// Helper to begin parameter gesture (for automation recording)
    pub fn begin_set_parameter(&self, param: &impl nih_plug::prelude::Param) {
        let setter = ParamSetter::new(&*self.context);
        setter.begin_set_parameter(param);
    }

    /// Helper to end parameter gesture
    pub fn end_set_parameter(&self, param: &impl nih_plug::prelude::Param) {
        let setter = ParamSetter::new(&*self.context);
        setter.end_set_parameter(param);
    }
}

impl<T: slint::ComponentHandle> baseview::WindowHandler for WindowHandler<T> {
    fn on_frame(&mut self, window: &mut baseview::Window) {
        // Explicitly make GL context current at the start of every frame
        unsafe {
            window.gl_context().unwrap().make_current();
        }

        // Mark GL context as ready - this allows renderer creation
        GL_CONTEXT_READY.with(|ready| *ready.borrow_mut() = true);

        // Show the Slint window on first frame when GL context is NOW active
        if !*self.window_shown.borrow() {
            // IMPORTANT: Initialize the renderer BEFORE calling show()
            // Context is current, so FemtoVG can query GL_VERSION
            let _ = self.adapter.renderer.get_or_init(|| {
                FemtoVGRenderer::new(BaseviewOpenGLInterface)
                    .expect("Failed to create FemtoVG renderer")
            });

            // Show the component's window (which uses our adapter)
            self.component.show().expect("Failed to show component");
            *self.window_shown.borrow_mut() = true;
        }

        // Call custom event loop handler first
        let setter = ParamSetter::new(&*self.context);
        (self.event_loop_handler)(&self, setter, window);

        // Update Slint timers and animations
        slint::platform::update_timers_and_animations();

        // Render the component - Slint handles the rendering internally
        // It will call our WindowAdapter's renderer() method when needed
        self.component.window().request_redraw();

        // Process Slint's internal rendering queue
        // This is where Slint actually renders using our FemtoVG renderer
        slint::platform::duration_until_next_timer_update();

        // CRITICAL: Actually trigger the render by accessing the renderer
        // Slint's FemtoVG renderer needs to be explicitly told to render
        if let Some(renderer) = self.adapter.renderer.get() {
            let _ = renderer.render();
        }

        // Process pending resizes
        self.process_pending_resizes(window);

        // Swap buffers after rendering
        window.gl_context().unwrap().swap_buffers();
    }

    fn on_event(&mut self, _window: &mut baseview::Window, event: Event) -> EventStatus {
        use slint::platform::WindowEvent;

        match event {
            Event::Mouse(mouse_event) => {
                // Convert baseview mouse event to Slint event
                let slint_event = match mouse_event {
                    baseview::MouseEvent::CursorMoved { position, .. } => {
                        let pos = LogicalPosition::new(position.x as f32, position.y as f32);
                        *self.last_cursor_pos.borrow_mut() = pos;
                        WindowEvent::PointerMoved { position: pos }
                    }
                    baseview::MouseEvent::ButtonPressed { button, .. } => {
                        let slint_button = match button {
                            baseview::MouseButton::Left => slint::platform::PointerEventButton::Left,
                            baseview::MouseButton::Right => slint::platform::PointerEventButton::Right,
                            baseview::MouseButton::Middle => slint::platform::PointerEventButton::Middle,
                            _ => return EventStatus::Ignored,
                        };
                        WindowEvent::PointerPressed {
                            button: slint_button,
                            position: *self.last_cursor_pos.borrow(),
                        }
                    }
                    baseview::MouseEvent::ButtonReleased { button, .. } => {
                        let slint_button = match button {
                            baseview::MouseButton::Left => slint::platform::PointerEventButton::Left,
                            baseview::MouseButton::Right => slint::platform::PointerEventButton::Right,
                            baseview::MouseButton::Middle => slint::platform::PointerEventButton::Middle,
                            _ => return EventStatus::Ignored,
                        };
                        WindowEvent::PointerReleased {
                            button: slint_button,
                            position: *self.last_cursor_pos.borrow(),
                        }
                    }
                    baseview::MouseEvent::WheelScrolled { delta, .. } => {
                        let (delta_x, delta_y) = match delta {
                            baseview::ScrollDelta::Lines { x, y } => (x * 20.0, y * 20.0),
                            baseview::ScrollDelta::Pixels { x, y } => (x, y),
                        };
                        WindowEvent::PointerScrolled {
                            position: LogicalPosition::new(0.0, 0.0),
                            delta_x: delta_x as f32,
                            delta_y: delta_y as f32,
                        }
                    }
                    _ => return EventStatus::Ignored,
                };

                self.adapter.window.dispatch_event(slint_event);
                EventStatus::Captured
            }
            Event::Keyboard(_key_event) => {
                // TODO: Implement keyboard event forwarding
                // This requires mapping baseview's keyboard types to Slint's
                EventStatus::Ignored
            }
            Event::Window(window_event) => {
                match window_event {
                    BaseviewWindowEvent::Resized(info) => {
                        // Handle scale factor and size changes from baseview
                        self.handle_window_info(&info);
                        EventStatus::Captured
                    }
                    BaseviewWindowEvent::Focused => EventStatus::Ignored,
                    BaseviewWindowEvent::Unfocused => EventStatus::Ignored,
                    BaseviewWindowEvent::WillClose => EventStatus::Ignored,
                }
            }
        }
    }
}

struct Instance {
    window_handle: WindowHandle,
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.window_handle.close();
    }
}

unsafe impl Send for Instance {}

impl<T: slint::ComponentHandle + 'static> Editor for SlintEditor<T> {
    fn spawn(
        &self,
        parent: nih_plug::prelude::ParentWindowHandle,
        context: Arc<dyn GuiContext>,
    ) -> Box<dyn std::any::Any + Send> {
        let options = WindowOpenOptions {
            scale: WindowScalePolicy::SystemScaleFactor,
            size: Size {
                width: self.width.load(Ordering::Relaxed) as f64,
                height: self.height.load(Ordering::Relaxed) as f64,
            },
            title: "Plug-in".to_owned(),
            // Request OpenGL context for FemtoVG rendering
            gl_config: Some(GlConfig {
                version: (3, 2),
                red_bits: 8,
                blue_bits: 8,
                green_bits: 8,
                alpha_bits: 8,
                depth_bits: 24,
                stencil_bits: 8,
                samples: None,
                srgb: true,
                double_buffer: true,
                vsync: false,
                ..Default::default()
            }),
        };

        let width = self.width.clone();
        let height = self.height.clone();
        let state = self.state.clone();
        let event_loop_handler = self.event_loop_handler.clone();
        let component_factory = self.component_factory.clone();

        let window_handle = baseview::Window::open_parented(&parent, options, move |baseview_window| {
            // CRITICAL FOR SLINT 1.14.1: Make GL context current BEFORE creating Slint components
            // Slint 1.14.1 tries to create the renderer during component initialization,
            // not lazily during first render as 1.8.0 did
            unsafe {
                baseview_window.gl_context().unwrap().make_current();
            }

            // Mark GL context as ready - allows renderer creation
            GL_CONTEXT_READY.with(|ready| *ready.borrow_mut() = true);

            // Create the Slint window adapter
            // Initially use scale factor 1.0 - this will be updated when we receive
            // the first WindowEvent::Resized from baseview with the actual scale factor
            let initial_scale = 1.0f32;
            let logical_width = width.load(Ordering::Relaxed);
            let logical_height = height.load(Ordering::Relaxed);
            // Start with logical = physical, will be corrected on first resize event
            let adapter = BaseviewSlintAdapter::new(
                logical_width,
                logical_height,
                initial_scale,
            );

            // Set the adapter in thread-local storage
            CURRENT_ADAPTER.with(|current| {
                *current.borrow_mut() = Some(adapter.clone());
            });

            // Set our custom platform - this tells Slint to use our adapter
            // On first open, this sets the platform; on subsequent opens,
            // the adapter is already updated above via thread-local storage
            let _ = slint::platform::set_platform(Box::new(BaseviewSlintPlatform));

            // Create the Slint component
            // NOTE: In Slint 1.14.1, this will trigger renderer() to be called immediately
            let component = component_factory()
                .unwrap_or_else(|e| panic!("Failed to create Slint component: {}", e));

            // Don't show the window yet - defer until first on_frame when GL context is active

            WindowHandler {
                context,
                event_loop_handler,
                width,
                height,
                scale_factor: RefCell::new(initial_scale),
                state,
                pending_resizes: Rc::new(RefCell::new(Vec::new())),
                last_cursor_pos: RefCell::new(LogicalPosition::new(0.0, 0.0)),
                window_shown: RefCell::new(false),
                component,
                adapter,
            }
        });

        Box::new(Instance { window_handle })
    }

    fn size(&self) -> (u32, u32) {
        (
            self.width.load(Ordering::Relaxed),
            self.height.load(Ordering::Relaxed),
        )
    }

    fn set_scale_factor(&self, _factor: f32) -> bool {
        // TODO: Implement scale factor handling for Slint
        false
    }

    fn param_values_changed(&self) {}

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {}

    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {}
}
