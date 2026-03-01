# nih-plug-slint

A high-performance adapter for using [Slint](https://slint.dev/) GUIs with [NIH-plug](https://github.com/robbert-vdh/nih-plug) audio plugins.

## Features

- **Instant Window Opening**: Native rendering eliminates 30-50ms webview initialization delay
- **GPU-Accelerated**: Uses FemtoVG renderer with OpenGL for smooth 60 FPS performance
- **State Persistence**: Automatic window size saving/loading across plugin instances
- **Bidirectional Parameter Sync**: Seamless Plugin ↔ UI communication
- **Thread-Safe**: Lock-free architecture for real-time audio thread safety
- **Cross-Platform**: Windows, macOS, Linux support via baseview
- **Window Reopen Support**: Fixed platform management allows reliable close/reopen cycles

## Quick Start

I went to the liberty of creating an example project that is just a simple Gain knob vst, using NIH-Plug and NIH-Plug-Slint

please see that here: []

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for detailed documentation on the system architecture, including:
- Component breakdown and responsibilities
- Thread-local platform management
- Lazy renderer initialization
- State persistence mechanism
- Data messaging system

## API Reference

### SlintEditor

```rust
impl<T: ComponentHandle + 'static> SlintEditor<T>
```

#### Methods

##### `with_factory(factory, size) -> Self`

Create a new editor with a component factory function.

**Arguments**:
- `factory`: `Fn() -> Result<T, PlatformError>` - Function to create Slint component
- `size`: `(u32, u32)` - Default window size (width, height) in physical pixels

**Example**:
```rust
SlintEditor::with_factory(
    || gui::AppWindow::new(),
    (800, 600)
)
```

##### `with_state(state) -> Self`

Attach persistent state for automatic window size saving/loading.

**Arguments**:
- `state`: `Arc<SlintEditorState>` - State from plugin params struct

**Example**:
```rust
.with_state(self.params.editor_state.clone())
```

##### `with_event_loop(handler) -> Self`

Set up the parameter synchronization callback.

**Arguments**:
- `handler`: `Fn(&WindowHandler<T>, ParamSetter, &mut Window)` - Called every frame

**Example**:
```rust
.with_event_loop(move |window_handler, _setter, _window| {
    let component = window_handler.component();

    // Update UI from parameters
    let value = params.some_param.unmodulated_normalized_value();
    component.set_some_property(value);

    // Set up UI → Plugin callbacks
    component.on_some_callback(move |value| {
        // Update parameter
    });
})
```

### WindowHandler

```rust
pub struct WindowHandler<T: ComponentHandle>
```

#### Methods

##### `component() -> &T`

Get reference to the Slint component instance.

##### `window() -> &slint::Window`

Get reference to the Slint window.

##### `context() -> &Arc<dyn GuiContext>`

Get reference to NIH-plug's GUI context for parameter operations.

##### `resize(window, width, height)`

Resize the window programmatically.

##### `queue_resize(width, height)`

Queue a resize to be processed later (avoids borrow checker issues).

### SlintEditorState

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlintEditorState
```

#### Fields

- `width: u32` - Window width in physical pixels
- `height: u32` - Window height in physical pixels
- `scale_factor: f32` - UI scale factor (1.0 = 100%)

#### Methods

##### `new(width, height) -> Self`

Create state with default scale factor (1.0).

##### `with_scale(width, height, scale_factor) -> Self`

Create state with custom scale factor.

## Slint UI Best Practices

### Adaptive Layouts

Slint's layout system automatically handles window resizing. Key points:

- **Use flexible layouts**: `VerticalBox`, `HorizontalLayout`, `GridLayout`
- **Set preferred sizes**: `preferred-width`, `preferred-height` for initial hints
- **Avoid fixed sizes**: Let widgets scale with container
- **Use min/max constraints**: `min-width`, `max-width` for bounds

**Example**:
```slint
export component AppWindow inherits Window {
    preferred-width: 400px;
    preferred-height: 300px;

    VerticalBox {
        padding: 20px;
        spacing: 10px;

        // Header: fixed height
        Text {
            text: "My Plugin";
            font-size: 24px;
        }

        // Controls: flex to fill space
        HorizontalLayout {
            spacing: 10px;
            Text { text: "Gain:"; vertical-alignment: center; }
            Slider { /* expands */ }
            Text { text: "50%"; min-width: 50px; }
        }
    }
}
```

### Aspect Ratio Preservation

When resizing, Slint layouts scale proportionally:

```slint
// Elements maintain relative sizes
VerticalBox {
    Rectangle { height: 30%; }  // Always 30% of container
    Rectangle { height: 70%; }  // Always 70% of container
}
```

### Property Binding

Use `<=>` for bidirectional binding:

```slint
Slider {
    value <=> root.gain-value;  // Bidirectional sync
    changed(value) => {
        root.gain-changed(value);  // Callback on change
    }
}
```

## Performance Tips

### UI Update Frequency

The event loop handler runs every frame (~16ms at 60 FPS). For expensive operations:

```rust
// Don't: Heavy computation every frame
.with_event_loop(move |handler, _, _| {
    let expensive_value = compute_expensive_thing();
    component.set_value(expensive_value);
})

// Do: Only update when value changes
.with_event_loop(move |handler, _, _| {
    let value = params.gain.unmodulated_normalized_value();
    if value != last_value.get() {
        component.set_value(value);
        last_value.set(value);
    }
})
```

### Programmatic Window Resizing

**Important:** Use the shared `pending_resizes` queue from callbacks:

```rust
// In event loop handler
component.on_resize_to_small({
    let pending_resizes = window_handler.pending_resizes().clone();
    move || {
        pending_resizes.borrow_mut().push((400, 300));
    }
});

component.on_resize_to_large({
    let pending_resizes = window_handler.pending_resizes().clone();
    move || {
        pending_resizes.borrow_mut().push((800, 600));
    }
});
```

**Why this pattern?**
- `pending_resizes()` returns `Rc<RefCell<Vec<(u32, u32)>>>`
- Cloning the `Rc` shares the same queue across all callbacks
- Resizes are processed on next frame by `process_pending_resizes()`
- This triggers Slint re-layout and forces immediate redraw

## Troubleshooting

### Window Opens But Shows Black Screen

**Cause**: Renderer not initialized (GL context not active)

**Solution**: Ensured by lazy initialization - automatically handled.

### Window Crashes on Reopen

**Cause**: Platform set multiple times

**Solution**: Thread-local adapter pattern - automatically handled.

### Parameters Not Updating

**Solution**: Ensure callbacks are set in event loop handler:

```rust
.with_event_loop(move |handler, _, _| {
    let component = handler.component();
    component.on_changed(move |v| { /* ... */ });
})
```

### Window Size Not Persisting

**Solution**:
1. Add state to params: `#[persist = "editor-state"] editor_state: Arc<SlintEditorState>`
2. Attach to editor: `.with_state(self.params.editor_state.clone())`

## Platform Support

### Windows
- OpenGL 3.2 Core Profile
- Tested on Windows 10/11

### macOS
- System OpenGL framework
- Tested on macOS 12+

### Linux
- X11 or Wayland
- Tested on Ubuntu 22.04+

## License

ISC License

## Credits

- Built on [NIH-plug](https://github.com/robbert-vdh/nih-plug) by Robbert van der Helm
- UI powered by [Slint](https://slint.dev/)
- Windowing via [baseview](https://github.com/RustAudio/baseview)