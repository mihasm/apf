//! Web views: the plugin's UI is HTML/CSS/JS shown in the system webview (WKWebView on
//! macOS, WebView2 on Windows, WebKitGTK on Linux) through `wry`, created as a child of the
//! window the host gives the plugin. Tauri itself cannot run inside a plugin because it owns
//! the window and the event loop; `wry` is the layer below it that can attach to a window it
//! does not own.
//!
//! The page is served from memory over a custom `apf://` scheme. It talks to the plugin
//! through three endpoints and one message channel, wrapped by the bundled `apf.js`:
//!
//! * `GET /__apf/apf.js` – the client library (`window.apf`), which also binds every element
//!   with a `data-param="key"` attribute to that parameter.
//! * `GET /__apf/info` – all parameters with their names, kinds, options and values.
//! * `GET /__apf/values?gen=N` – current values and texts, the current preset and the MIDI
//!   learn state, or `204` when nothing changed since generation `N`.
//! * `GET /__apf/presets` – the preset list, each with its type, subtype, mood words,
//!   description and author; `/__apf/preset/load?id=`,
//!   `/__apf/preset/save?name=&overwrite=0|1[&type=&subtype=&mood=a,b&description=&author=]`
//!   (without any of the metadata, an overwritten preset keeps its own),
//!   `/__apf/preset/delete?id=` act on it.
//! * `GET /__apf/learn?index=i` (-1 cancels), `/__apf/forget?index=i` – MIDI learn.
//! * `GET`/`POST /__apf/plugin/<path>?<query>` – the plugin's own requests, answered by its
//!   [`apf::Controller`] (`apf.plugin.get/post/raw`); 404 when it has none. `values` also
//!   carries the controller's revision (`rev`), so the page hears of changes from elsewhere.
//! * `POST /__apf/echo` – answers the body's length and FNV-1a hash; `apf-host --gui-test`
//!   checks with it that the webview delivers request bodies intact.
//! * `window.ipc.postMessage("begin i" | "set i normalized" | "end i")` – edits;
//!   `"log text"` writes a line to the plugin's stderr (`apf.log`).
//!
//! Request bodies: wry hands the custom protocol handler the body of a `fetch` POST on every
//! platform (on macOS from `NSURLRequest.HTTPBody`, or its body stream), so JSON and binary
//! bodies reach the controller as sent; `apf-host --gui-test` checks a 1 MiB body in
//! WKWebView.
//!
//! Setting the environment variable `APF_UI_DIR` to a folder serves the page from disk
//! instead, so a UI can be edited and reloaded without rebuilding the plugin.
//! `APF_UI_INJECT_JS` names a script file run in every page before its own scripts; the
//! test host uses it to drive the UI.

use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt::Write as _;
use std::num::NonZeroIsize;
use std::path::PathBuf;
use std::rc::Rc;

use apf::controller::query_arg;
use apf::preset::{Metadata, SaveError, Source};
use apf::{ControllerRequest, View, ViewContext, ParamKind, ParentWindow};
use raw_window_handle::{
    AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
    XlibWindowHandle,
};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::{header::CONTENT_TYPE, Request, Response, StatusCode};
use wry::{Rect, WebViewBuilder};

const SCHEME: &str = "apf";
const CLIENT_JS: &[u8] = include_bytes!("apf.js");

/// A file of the UI, embedded in the plugin binary.
#[derive(Debug, Clone, Copy)]
pub struct Asset {
    /// Path below the root, without a leading slash: "index.html", "img/knob.svg".
    pub path: &'static str,
    pub bytes: &'static [u8],
}

/// Embed UI files: `apf_webview::assets!["index.html" => "../ui/index.html", ...]`, paths
/// relative to the source file, like `include_bytes!`.
#[macro_export]
macro_rules! assets {
    ($($path:literal => $file:literal),* $(,)?) => {
        &[$($crate::Asset { path: $path, bytes: include_bytes!($file) }),*]
    };
}

/// A [`View`] showing a web page: the plugin's web view.
pub struct WebView {
    width: u32,
    height: u32,
    resizable: bool,
    min_size: (u32, u32),
    max_size: (u32, u32),
    background: (u8, u8, u8, u8),
    assets: &'static [Asset],
    webview: Option<Rc<wry::WebView>>,
    #[cfg(target_os = "macos")]
    staging: Option<mac::Staging>,
    /// The scale the host set (CLAP `set_scale`); without it, the DPI of the parent window.
    #[cfg(target_os = "windows")]
    host_scale: Option<f64>,
    /// The host's window (HWND), while the web view is open.
    #[cfg(target_os = "windows")]
    parent: Option<isize>,
    /// A resizable web view follows its parent window's client area.
    #[cfg(target_os = "windows")]
    follower: Option<win::Follower>,
    /// WebView2's user data folder (its default, beside the host's executable, is not writable).
    #[cfg(target_os = "windows")]
    web_context: Option<wry::WebContext>,
    /// A resizable web view follows its parent view's frame.
    #[cfg(target_os = "macos")]
    follower: Option<mac::Follower>,
    /// What the page told the plugin (claimed shortcuts), and the way back to its view.
    page: Rc<PageLink>,
    #[cfg(target_os = "macos")]
    key_monitor: Option<mac::KeyMonitor>,
}

/// State shared by the web view and the page's message handler (main thread only).
#[derive(Default)]
struct PageLink {
    claims: Rc<RefCell<keys::Claims>>,
    #[cfg(target_os = "macos")]
    view: RefCell<Option<objc2::rc::Weak<objc2_app_kit::NSView>>>,
    /// The host's window (HWND) and the web view's scale, for keys and `maximize`.
    #[cfg(target_os = "windows")]
    parent: std::cell::Cell<isize>,
    #[cfg(target_os = "windows")]
    scale: std::cell::Cell<f64>,
}

impl WebView {
    /// `assets` must contain `index.html`.
    pub fn new(width: u32, height: u32, assets: &'static [Asset]) -> Self {
        Self {
            width,
            height,
            resizable: false,
            min_size: (160, 120),
            max_size: (16384, 16384),
            background: (24, 24, 28, 255),
            assets,
            webview: None,
            #[cfg(target_os = "macos")]
            staging: None,
            #[cfg(target_os = "macos")]
            follower: None,
            #[cfg(target_os = "windows")]
            host_scale: None,
            #[cfg(target_os = "windows")]
            parent: None,
            #[cfg(target_os = "windows")]
            follower: None,
            #[cfg(target_os = "windows")]
            web_context: None,
            page: Rc::new(PageLink::default()),
            #[cfg(target_os = "macos")]
            key_monitor: None,
        }
    }

    /// Let the user resize the web view (hosts show a resizable window). The page follows the
    /// window: lay it out for any size (CSS `100vw`/`100vh`, flex or grid), or listen to
    /// `resize`. The framework keeps the size the user chose with the session.
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    /// The smallest and largest size a resizable web view takes, in logical pixels (default
    /// 160 × 120 and no practical maximum).
    pub fn size_limits(mut self, min: (u32, u32), max: (u32, u32)) -> Self {
        self.min_size = (min.0.max(1), min.1.max(1));
        self.max_size = (max.0.max(self.min_size.0), max.1.max(self.min_size.1));
        self
    }

    /// Open at the size of the screen, less `margin` (room for the host window's title bar and
    /// toolbar), within the size limits — set the limits first. For hosts that let the web view
    /// propose its size but not change it afterwards (Logic with an AUv3). A size the user
    /// chose in a session still wins. Keeps the size given to `new` where the screen is not
    /// known (Linux, not on the main thread). On Windows the screen is the primary monitor's
    /// work area (without the taskbar).
    pub fn fit_screen(mut self, margin: (u32, u32)) -> Self {
        #[cfg(target_os = "macos")]
        let room = mac::main_screen_room();
        #[cfg(target_os = "windows")]
        let room = win::primary_work_area();
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let room: Option<(f64, f64)> = None;
        if let Some((w, h)) = room {
            let (w, h) = ((w as u32).saturating_sub(margin.0), (h as u32).saturating_sub(margin.1));
            self.width = w.clamp(self.min_size.0, self.max_size.0);
            self.height = h.clamp(self.min_size.1, self.max_size.1);
        }
        self
    }

    /// Colour shown before the page has painted, so the window does not flash white.
    pub fn background(mut self, r: u8, g: u8, b: u8) -> Self {
        self.background = (r, g, b, 255);
        self
    }

    /// Connect the page's link to the built webview: claimed keys, its view for `maximize`.
    fn attach_page(&mut self, _webview: &wry::WebView) {
        #[cfg(target_os = "macos")]
        {
            use wry::WebViewExtMacOS;
            *self.page.view.borrow_mut() = Some(objc2::rc::Weak::new(&*_webview.webview()));
            self.key_monitor = mac::KeyMonitor::new(_webview, self.page.claims.clone());
        }
    }

    fn bounds(&self) -> Rect {
        // Windows: the host's window counts physical pixels; give wry those, with the scale the
        // host agreed to (wry would convert with the window's DPI).
        #[cfg(target_os = "windows")]
        {
            let s = self.scale();
            let size = ((self.width as f64 * s).round() as u32, (self.height as f64 * s).round() as u32);
            return Rect { position: wry::dpi::PhysicalPosition::new(0, 0).into(), size: wry::dpi::PhysicalSize::new(size.0, size.1).into() };
        }
        #[allow(unreachable_code)]
        Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(self.width as f64, self.height as f64).into(),
        }
    }
}

struct Parent(ParentWindow);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = match self.0 {
            ParentWindow::AppKit(view) => RawWindowHandle::AppKit(AppKitWindowHandle::new(view)),
            ParentWindow::Win32(hwnd) => {
                let hwnd = NonZeroIsize::new(hwnd.as_ptr() as isize).ok_or(HandleError::Unavailable)?;
                RawWindowHandle::Win32(Win32WindowHandle::new(hwnd))
            }
            ParentWindow::X11(id) => RawWindowHandle::Xlib(XlibWindowHandle::new(id as _)),
        };
        // SAFETY: the host keeps the parent window alive while the web view is open, and the
        // webview is destroyed in `close`, before the host may destroy it.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl View for WebView {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[cfg(target_os = "windows")]
    fn scale(&self) -> f64 {
        self.host_scale.unwrap_or_else(|| win::scale_of(self.parent))
    }

    #[cfg(target_os = "windows")]
    fn set_scale(&mut self, scale: f64) -> bool {
        if !(scale.is_finite() && scale > 0.0) {
            return false;
        }
        self.host_scale = Some(scale);
        self.page.scale.set(scale);
        if let Some(webview) = &self.webview {
            let _ = webview.set_bounds(self.bounds());
        }
        if let Some(f) = &self.follower {
            f.set_scale(scale);
        }
        true
    }

    fn resizable(&self) -> bool {
        self.resizable
    }

    fn adjust_size(&self, width: u32, height: u32) -> (u32, u32) {
        (width.clamp(self.min_size.0, self.max_size.0), height.clamp(self.min_size.1, self.max_size.1))
    }

    fn set_size(&mut self, width: u32, height: u32) -> bool {
        if !self.resizable {
            return false;
        }
        let (width, height) = self.adjust_size(width, height);
        self.width = width;
        self.height = height;
        if let Some(webview) = &self.webview {
            #[cfg(target_os = "macos")]
            match &self.follower {
                Some(f) => f.layout(),
                None => mac::set_frame(webview, width, height),
            }
            #[cfg(not(target_os = "macos"))]
            let _ = webview.set_bounds(self.bounds());
        }
        true
    }

    fn open(&mut self, parent: ParentWindow, ctx: ViewContext) -> Result<(), String> {
        self.close();
        let assets = self.assets;
        let dev_dir = std::env::var_os("APF_UI_DIR").map(PathBuf::from);
        let protocol_ctx = ctx.clone();
        let ipc_ctx = ctx;
        let sizing = Sizing { resizable: self.resizable, min: self.min_size, max: self.max_size };
        self.page = Rc::new(PageLink::default());
        let page = self.page.clone();

        #[cfg(target_os = "windows")]
        let mut web_context = {
            let ParentWindow::Win32(hwnd) = parent else { return Err("the web view needs a Win32 window".into()) };
            self.parent = Some(hwnd.as_ptr() as isize);
            page.parent.set(hwnd.as_ptr() as isize);
            page.scale.set(self.scale());
            // WebView2's loader is linked in statically (webview2-com-sys, MSVC).
            wry::WebContext::new(win::data_dir(protocol_ctx.plugin_info()))
        };
        #[cfg(target_os = "windows")]
        let mut builder = {
            use wry::WebViewBuilderExtWindows;
            // No reload on F5/Ctrl+R, no find bar on Ctrl+F (the page's own shortcuts), and the
            // page's context menus only (the browser's adds "Inspect" where devtools are on).
            WebViewBuilder::new_with_web_context(&mut web_context)
                .with_browser_accelerator_keys(false)
                .with_default_context_menus(cfg!(any(debug_assertions, feature = "devtools")))
        };
        #[cfg(not(target_os = "windows"))]
        let mut builder = WebViewBuilder::new();
        if let Some(script) = std::env::var_os("APF_UI_INJECT_JS").and_then(|p| std::fs::read_to_string(p).ok()) {
            builder = builder.with_initialization_script(script);
        }
        let builder = builder
            .with_bounds(self.bounds())
            .with_background_color(self.background)
            .with_accept_first_mouse(true)
            .with_devtools(cfg!(any(debug_assertions, feature = "devtools")))
            .with_custom_protocol(SCHEME.into(), move |_id, request| {
                serve(&protocol_ctx, &sizing, assets, dev_dir.as_deref(), &request)
            })
            .with_ipc_handler(move |request: Request<String>| handle_message(&ipc_ctx, &sizing, &page, request.body()))
            .with_url(page_url("index.html"));

        // AU hosts (Logic among them) hand the plugin a view that is not in a window yet, and
        // wry needs one to build the webview. Build it in a hidden staging window then and
        // move it into the host's view.
        #[cfg(target_os = "macos")]
        if let ParentWindow::AppKit(view) = parent {
            if !unsafe { mac::in_window(view) } {
                let staging = mac::Staging::new(self.width, self.height)?;
                let webview = builder.build_as_child(&Parent(ParentWindow::AppKit(staging.view()))).map_err(|e| e.to_string())?;
                unsafe { mac::adopt(&webview, view, self.width, self.height) };
                if self.resizable {
                    self.follower = mac::Follower::new(&webview, self.min_size, self.max_size);
                }
                self.attach_page(&webview);
                self.webview = Some(Rc::new(webview));
                self.staging = Some(staging);
                return Ok(());
            }
        }

        let webview = Rc::new(builder.build_as_child(&Parent(parent)).map_err(|e| e.to_string())?);
        // wry pins a child webview's size; a resizable web view follows its parent view also
        // when a host resizes that without calling `set_size`.
        #[cfg(target_os = "macos")]
        if self.resizable {
            self.follower = mac::Follower::new(&webview, self.min_size, self.max_size);
        }
        #[cfg(target_os = "windows")]
        {
            self.web_context = Some(web_context);
            if self.resizable {
                if let Some(hwnd) = self.parent {
                    self.follower = win::Follower::new(hwnd, &webview, self.min_size, self.max_size, self.scale());
                }
            }
        }
        self.attach_page(&webview);
        self.webview = Some(webview);
        Ok(())
    }

    fn close(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.follower = None;
            self.key_monitor = None;
        }
        // Before the webview goes: the follower holds the host's window subclassed.
        #[cfg(target_os = "windows")]
        {
            self.follower = None;
        }
        // Dropping the webview removes it from the parent view.
        self.webview = None;
        #[cfg(target_os = "macos")]
        {
            self.staging = None;
        }
        #[cfg(target_os = "windows")]
        {
            self.web_context = None;
            self.parent = None;
        }
    }

    fn set_visible(&mut self, visible: bool) {
        if let Some(webview) = &self.webview {
            let _ = webview.set_visible(visible);
        }
    }
}

pub mod keys;

#[cfg(target_os = "windows")]
mod win {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::{Rc, Weak};

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
    use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC};
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        AdjustWindowRectEx, GetAncestor, GetClientRect, GetWindowRect, PostMessageW, GA_ROOT, WINDOW_EX_STYLE, WM_KEYDOWN, WM_KEYUP, WM_NCDESTROY, WM_SIZE, WM_SYSKEYDOWN, WM_SYSKEYUP,
        WS_OVERLAPPEDWINDOW,
    };
    use wry::dpi::{PhysicalPosition, PhysicalSize};
    use wry::{Rect, WebView};

    fn hwnd(h: isize) -> HWND {
        HWND(h as *mut _)
    }

    /// Physical pixels per logical pixel: the DPI of the host's window, before it is known the
    /// system's.
    pub fn scale_of(parent: Option<isize>) -> f64 {
        // SAFETY: plain Win32 getters; a stale handle answers 0.
        let dpi = parent.map_or(0, |p| unsafe { GetDpiForWindow(hwnd(p)) });
        let dpi = if dpi == 0 { unsafe { GetDpiForSystem() } } else { dpi };
        if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 }
    }

    /// WebView2's user data folder for this plugin: `%LOCALAPPDATA%\<vendor>\<plugin>\WebView2`.
    pub fn data_dir(info: &apf::PluginInfo) -> Option<PathBuf> {
        let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
        Some(base.join(apf::preset::file_stem_of(info.vendor)).join(apf::preset::file_stem_of(info.name)).join("WebView2"))
    }

    fn work_area(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<RECT> {
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: `info` is a MONITORINFO with its size set.
        unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool().then_some(info.rcWork)
    }

    /// The primary monitor without the taskbar, less the frame of a window around the web view
    /// (hosts put it in a window of their own), in logical pixels.
    pub fn primary_work_area() -> Option<(f64, f64)> {
        // SAFETY: plain Win32 getters.
        let r = work_area(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) })?;
        let mut frame = RECT::default();
        // SAFETY: computes the frame of a standard window around an empty client area.
        let (fw, fh) = match unsafe { AdjustWindowRectEx(&mut frame, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE::default()) } {
            Ok(()) => (frame.right - frame.left, frame.bottom - frame.top),
            Err(_) => (0, 0),
        };
        let s = scale_of(None);
        Some((((r.right - r.left) - fw) as f64 / s, ((r.bottom - r.top) - fh) as f64 / s))
    }

    /// The largest web view the monitor of the host's window has room for, in logical pixels:
    /// its work area less the host window's frame around the web view (title bar, menus, an FX
    /// list beside it).
    pub fn screen_room(parent: isize, scale: f64) -> Option<(f64, f64)> {
        if parent == 0 || scale <= 0.0 {
            return None;
        }
        let parent = hwnd(parent);
        // SAFETY: Win32 getters on the host's window, alive while the web view is open.
        unsafe {
            let work = work_area(MonitorFromWindow(parent, MONITOR_DEFAULTTONEAREST))?;
            let (mut client, mut frame) = (RECT::default(), RECT::default());
            GetClientRect(parent, &mut client).ok()?;
            let root = GetAncestor(parent, GA_ROOT);
            let (mut cw, mut ch) = (0, 0);
            if !root.is_invalid() && GetWindowRect(root, &mut frame).is_ok() {
                let (w, h) = ((frame.right - frame.left) - (client.right - client.left), (frame.bottom - frame.top) - (client.bottom - client.top));
                // A docked web view in a host's main window has no frame of its own to speak of.
                if (0..1000).contains(&w) && (0..600).contains(&h) {
                    (cw, ch) = (w, h);
                }
            }
            Some((((work.right - work.left) - cw) as f64 / scale, ((work.bottom - work.top) - ch) as f64 / scale))
        }
    }

    /// Hand a key the page did not take to the host's window, as if it had been typed there:
    /// posted, so it passes through the host's message loop and its key commands (the space
    /// bar, Ctrl+S). The webview's own process gets every key while it has the focus, so the
    /// host would never see it otherwise.
    pub fn forward_key(parent: isize, down: bool, code: u32, alt: bool) {
        if parent == 0 || code == 0 || code > 0xfe {
            return;
        }
        // SAFETY: a lookup and a posted message to the host's window.
        unsafe {
            let scan = MapVirtualKeyW(code, MAPVK_VK_TO_VSC);
            let mut lparam: u32 = 1 | (scan & 0xff) << 16;
            if alt {
                lparam |= 1 << 29;
            }
            if !down {
                lparam |= 3 << 30;
            }
            let msg = match (down, alt) {
                (true, false) => WM_KEYDOWN,
                (false, false) => WM_KEYUP,
                (true, true) => WM_SYSKEYDOWN,
                (false, true) => WM_SYSKEYUP,
            };
            let _ = PostMessageW(Some(hwnd(parent)), msg, WPARAM(code as usize), LPARAM(lparam as isize));
        }
    }

    struct Follow {
        parent: HWND,
        webview: Weak<WebView>,
        min: (u32, u32),
        max: (u32, u32),
        scale: Cell<f64>,
    }

    /// Keeps a resizable web view filling the host's window (its client area), within
    /// the web view's limits, whenever the host resizes it — with or without `set_size`, before
    /// or after it. The host's window is subclassed while the web view is open.
    pub struct Follower {
        state: Box<Follow>,
    }

    impl Follower {
        pub fn new(parent: isize, webview: &Rc<WebView>, min: (u32, u32), max: (u32, u32), scale: f64) -> Option<Follower> {
            let state = Box::new(Follow { parent: hwnd(parent), webview: Rc::downgrade(webview), min, max, scale: Cell::new(scale) });
            let data = &*state as *const Follow as usize;
            // SAFETY: `state` outlives the subclass: Drop removes it before freeing the box.
            let ok = unsafe { SetWindowSubclass(state.parent, Some(follow_proc), data, data) }.as_bool();
            ok.then(|| {
                fill(&state);
                Follower { state }
            })
        }

        pub fn set_scale(&self, scale: f64) {
            self.state.scale.set(scale);
            fill(&self.state);
        }
    }

    impl Drop for Follower {
        fn drop(&mut self) {
            let data = &*self.state as *const Follow as usize;
            // SAFETY: removes the subclass added in `new` (a no-op once the window is gone).
            unsafe {
                let _ = RemoveWindowSubclass(self.state.parent, Some(follow_proc), data);
            }
        }
    }

    unsafe extern "system" fn follow_proc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, id: usize, data: usize) -> LRESULT {
        match msg {
            WM_SIZE => {
                let result = DefSubclassProc(window, msg, wparam, lparam);
                fill(&*(data as *const Follow));
                return result;
            }
            WM_NCDESTROY => {
                let _ = RemoveWindowSubclass(window, Some(follow_proc), id);
            }
            _ => {}
        }
        DefSubclassProc(window, msg, wparam, lparam)
    }

    fn fill(s: &Follow) {
        let Some(webview) = s.webview.upgrade() else { return };
        let mut r = RECT::default();
        // SAFETY: a getter on the host's window.
        if unsafe { GetClientRect(s.parent, &mut r) }.is_err() {
            return;
        }
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w < 1 || h < 1 {
            return;
        }
        let sc = s.scale.get();
        let px = |v: u32| (v as f64 * sc).round() as i32;
        let w = w.clamp(px(s.min.0), px(s.max.0).max(px(s.min.0)));
        let h = h.clamp(px(s.min.1), px(s.max.1).max(px(s.min.1)));
        let _ = webview.set_bounds(Rect { position: PhysicalPosition::new(0, 0).into(), size: PhysicalSize::new(w as u32, h as u32).into() });
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use std::cell::RefCell;
    use std::rc::Rc;

    use block2::RcBlock;
    use objc2::rc::{Retained, Weak};
    use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSResponder, NSScreen, NSView,
        NSViewFrameDidChangeNotification, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSArray, NSNotification, NSNotificationCenter, NSPoint, NSRect, NSRunLoopCommonModes, NSSize};

    use crate::keys::{Claims, Key, KeyCombo};
    use wry::{WebView, WebViewExtMacOS};

    /// # Safety
    /// `view` is a live `NSView*`, used on the main thread.
    pub unsafe fn in_window(view: NonNull<c_void>) -> bool {
        let view = &*(view.as_ptr() as *const NSView);
        view.window().is_some()
    }

    /// A hidden window the webview is created in when the host's view has none yet.
    pub struct Staging {
        window: Retained<NSWindow>,
        content: Retained<NSView>,
    }

    impl Staging {
        pub fn new(width: u32, height: u32) -> Result<Self, String> {
            let mtm = MainThreadMarker::new().ok_or("the web view must be opened on the main thread")?;
            let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width as f64, height as f64));
            // SAFETY: plain AppKit calls on the main thread.
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    mtm.alloc(),
                    rect,
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    true,
                )
            };
            unsafe { window.setReleasedWhenClosed(false) };
            let content = window.contentView().ok_or("the staging window has no content view")?;
            Ok(Self { window, content })
        }

        pub fn view(&self) -> NonNull<c_void> {
            NonNull::from(&*self.content).cast()
        }
    }

    impl Drop for Staging {
        fn drop(&mut self) {
            self.window.close();
        }
    }

    /// Move the webview into the host's view, filling it.
    ///
    /// # Safety
    /// `parent` is a live `NSView*`, used on the main thread.
    pub unsafe fn adopt(webview: &WebView, parent: NonNull<c_void>, width: u32, height: u32) {
        let parent = &*(parent.as_ptr() as *const NSView);
        let view = webview.webview();
        view.removeFromSuperview();
        parent.addSubview(&view);
        view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        set_frame(webview, width, height);
    }

    /// Keeps a webview filling its parent view, within the web view's size limits, whenever the
    /// parent's frame changes. Hosts resize their view before `set_size` (clap-wrapper's AUv2
    /// and VST3), after it (REAPER's CLAP host), or without it, and may make it smaller than
    /// the web view's minimum (REAPER): AppKit's autoresizing gets the first two wrong in one of
    /// the orders (it adds the growth a second time) and squeezes the page in the last.
    pub struct Follower {
        token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
        view: Weak<NSView>,
        limits: Limits,
    }

    #[derive(Clone, Copy)]
    struct Limits {
        min: (u32, u32),
        max: (u32, u32),
    }

    impl Follower {
        pub fn new(webview: &WebView, min: (u32, u32), max: (u32, u32)) -> Option<Follower> {
            let view: &NSView = &webview.webview();
            // SAFETY: AppKit calls on the main thread, where the web view lives.
            let parent = unsafe { view.superview() }?;
            view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewNotSizable);
            parent.setPostsFrameChangedNotifications(true);
            let limits = Limits { min, max };
            let weak = Weak::new(view);
            let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
                if let Some(view) = weak.load() {
                    fill(&view, limits);
                    keep_on_screen(&view);
                }
            });
            // SAFETY: the notification is posted on the main thread (by the frame change) and
            // the block runs there, synchronously (no queue); the observer is removed in Drop.
            let token = unsafe {
                NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                    Some(NSViewFrameDidChangeNotification),
                    Some(&parent),
                    None,
                    &block,
                )
            };
            fill(view, limits);
            Some(Follower { token, view: Weak::new(view), limits })
        }

        /// Lay the webview out again (after `set_size`).
        pub fn layout(&self) {
            if let Some(view) = self.view.load() {
                fill(&view, self.limits);
            }
        }
    }

    impl Drop for Follower {
        fn drop(&mut self) {
            // SAFETY: the token came from addObserverForName on this center.
            unsafe { NSNotificationCenter::defaultCenter().removeObserver(self.token.as_ref()) };
        }
    }

    /// The parent's size within the limits, the page's top edge at the parent's top.
    fn fill(view: &NSView, limits: Limits) {
        // SAFETY: AppKit getters on the main thread.
        let Some(parent) = (unsafe { view.superview() }) else { return };
        let size = parent.bounds().size;
        if size.width < 1.0 || size.height < 1.0 {
            return;
        }
        let w = size.width.clamp(limits.min.0 as f64, limits.max.0 as f64);
        let h = size.height.clamp(limits.min.1 as f64, limits.max.1 as f64);
        let y = if parent.isFlipped() { 0.0 } else { size.height - h };
        view.setFrame(NSRect::new(NSPoint::new(0.0, y), NSSize::new(w, h)));
    }

    /// The shortcut a key event is, if it is one a page can claim.
    fn combo_of(event: &NSEvent) -> Option<KeyCombo> {
        let flags = event.modifierFlags();
        let key = match event.keyCode() {
            51 => Key::Backspace,
            117 => Key::Delete,
            53 => Key::Escape,
            49 => Key::Space,
            36 | 76 => Key::Enter,
            48 => Key::Tab,
            123 => Key::Left,
            124 => Key::Right,
            125 => Key::Down,
            126 => Key::Up,
            115 => Key::Home,
            119 => Key::End,
            116 => Key::PageUp,
            121 => Key::PageDown,
            _ => {
                let text = event.charactersIgnoringModifiers()?.to_string().to_lowercase();
                let mut chars = text.chars();
                let c = chars.next()?;
                if chars.next().is_some() || c.is_control() {
                    return None;
                }
                Key::Char(c)
            }
        };
        Some(KeyCombo {
            cmd: flags.contains(NSEventModifierFlags::Command),
            shift: flags.contains(NSEventModifierFlags::Shift),
            alt: flags.contains(NSEventModifierFlags::Option),
            ctrl: flags.contains(NSEventModifierFlags::Control),
            key,
        })
    }

    /// Routes the keyboard between the page and the host while the keyboard focus is in the
    /// webview (an application-wide key monitor, which sees every key before the host's own
    /// handling; in this process only, so in-process hosts).
    ///
    /// * Claimed shortcuts go to the page first, before the host's key commands.
    /// * A key the page does not take reaches the host whole. WebKit hands an unhandled
    ///   key-down back to the application (`NSApp sendEvent:` with the same event, which the
    ///   monitor sees a second time) and from there it goes up the responder chain to the
    ///   host — but it drops every key-up and modifier change once the page has seen it. The
    ///   host then got the key-down alone: Logic's Musical Typing held the note forever. So
    ///   the key-up of a key whose key-down went to the host goes up the same chain beside
    ///   its way to the page, and so does every modifier change; neither is held back from
    ///   the page, so both sides see the same modifier state. WebKit decides asynchronously:
    ///   a short press's key-up can come before the key-down is handed back, and then waits
    ///   for it.
    pub struct KeyMonitor {
        monitor: Retained<AnyObject>,
    }

    /// A key-down handed to the page: its (timestamp, key code), the key's latest press, and
    /// its key-up when that came before the page decided.
    struct Pending {
        id: (f64, u16),
        up: Option<Retained<NSEvent>>,
    }

    impl KeyMonitor {
        pub fn new(webview: &WebView, claims: Rc<RefCell<Claims>>) -> Option<KeyMonitor> {
            let mtm = MainThreadMarker::new()?;
            let view = Weak::new(&*webview.webview());
            // One per key: the key-downs WebKit has not handed back (yet, or ever: the page's).
            let pending: RefCell<Vec<Pending>> = RefCell::new(Vec::new());
            // Key codes whose key-down the page left to the host, waiting for their key-up.
            let to_host: RefCell<Vec<u16>> = RefCell::new(Vec::new());
            let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
                // SAFETY: AppKit hands the monitor a live event on the main thread.
                let ev = unsafe { event.as_ref() };
                let pass = event.as_ptr();
                let Some(view) = view.load() else { return pass };
                // The host's side of the webview: where WebKit's super-class handling sends a
                // key on (its superview, the view the host gave the web view).
                // SAFETY: an AppKit getter on the main thread.
                let host = || unsafe { view.nextResponder() };
                let kind = ev.r#type();
                let code = if kind == NSEventType::FlagsChanged { 0 } else { ev.keyCode() };
                // A key-up ends the key's trip to the host, wherever the focus went meanwhile.
                let mut up_to_host = kind == NSEventType::KeyUp && {
                    let mut to_host = to_host.borrow_mut();
                    let at = to_host.iter().position(|&c| c == code);
                    at.map(|i| to_host.swap_remove(i)).is_some()
                };
                let (Some(window), Some(ev_window)) = (view.window(), ev.window(mtm)) else { return pass };
                if !std::ptr::eq(&*window, &*ev_window) {
                    return pass;
                }
                // The focus must be in the webview (WebKit's own views live inside it).
                let Some(focus) = window.firstResponder() else { return pass };
                if !focus.downcast_ref::<NSView>().is_some_and(|r| r.isDescendantOf(&view)) {
                    return pass;
                }
                match kind {
                    NSEventType::KeyDown => {
                        let id = (ev.timestamp(), code);
                        let mut pending = pending.borrow_mut();
                        if let Some(i) = pending.iter().position(|p| p.id == id) {
                            // WebKit handing back what the page did not take: the host's turn.
                            let p = pending.swap_remove(i);
                            match (p.up, host()) {
                                // Released already: the key-up follows this key-down.
                                (Some(up), Some(host)) => key_up_later(&host, &up),
                                (Some(_), None) => {}
                                (None, _) => to_host.borrow_mut().push(code),
                            }
                            return pass;
                        }
                        // A new press (an older one of the key the page took, or one whose
                        // hand-back a key repeat overtook, is not waited for).
                        pending.retain(|p| p.id.1 != code);
                        pending.push(Pending { id, up: None });
                        drop(pending);
                        let claimed = combo_of(ev).is_some_and(|combo| claims.borrow().contains(&combo));
                        if !claimed {
                            return pass;
                        }
                        // A key window's WebKit takes it as a key equivalent; otherwise as a key press.
                        if !view.performKeyEquivalent(ev) {
                            view.keyDown(ev);
                        }
                        std::ptr::null_mut()
                    }
                    NSEventType::KeyUp => {
                        if let Some(p) = pending.borrow_mut().iter_mut().find(|p| p.id.1 == code && p.up.is_none()) {
                            // The page has not decided on the key-down yet: the host gets this
                            // key-up after the key-down, if at all.
                            p.up = Some(objc2::Message::retain(ev));
                            up_to_host = false;
                        }
                        if let (true, Some(host)) = (up_to_host, host()) {
                            host.keyUp(ev);
                        }
                        pass
                    }
                    NSEventType::FlagsChanged => {
                        if let Some(host) = host() {
                            host.flagsChanged(ev);
                        }
                        pass
                    }
                    _ => pass,
                }
            });
            let mask = NSEventMask::KeyDown | NSEventMask::KeyUp | NSEventMask::FlagsChanged;
            // SAFETY: the block runs on the main thread for this process's events; the
            // monitor is removed in Drop.
            let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) }?;
            Some(KeyMonitor { monitor })
        }
    }

    /// Hand `up` to `host` once the key-down being sent now has reached it (on the next turn
    /// of the run loop, in any of its common modes: also while the host tracks a menu or a drag).
    fn key_up_later(host: &NSResponder, up: &NSEvent) {
        // SAFETY: NSObject's performSelector on the main thread; it retains the target and the
        // event until it runs.
        unsafe {
            let modes = NSArray::from_slice(&[NSRunLoopCommonModes]);
            let _: () = objc2::msg_send![host, performSelector: objc2::sel!(keyUp:), withObject: up, afterDelay: 0.0f64, inModes: &*modes];
        }
    }

    impl Drop for KeyMonitor {
        fn drop(&mut self) {
            // SAFETY: the monitor came from addLocalMonitorForEventsMatchingMask.
            unsafe { NSEvent::removeMonitor(&self.monitor) };
        }
    }

    /// The main screen without the menu bar and Dock, in points; `None` off the main thread.
    pub fn main_screen_room() -> Option<(f64, f64)> {
        let screen = NSScreen::mainScreen(MainThreadMarker::new()?)?;
        let size = screen.visibleFrame().size;
        Some((size.width, size.height))
    }

    /// The largest size the web view can take on the screen it is on: the screen without the
    /// menu bar and Dock, less the host window's frame around the web view (its title bar and
    /// toolbar) when the web view is in a window of its own. `None` before the web view is in a
    /// window.
    pub fn screen_room(view: &NSView) -> Option<(f64, f64)> {
        // SAFETY: AppKit getters on the main thread.
        let parent = unsafe { view.superview() }?;
        let window = view.window()?;
        let screen = window.screen().or_else(|| NSScreen::mainScreen(MainThreadMarker::new()?))?;
        let room = screen.visibleFrame().size;
        let (chrome_w, chrome_h) = chrome(&window, &parent).unwrap_or((0.0, 0.0));
        Some((room.width - chrome_w, room.height - chrome_h))
    }

    /// The host window's frame around the web view, when the window is the web view's own
    /// (a docked web view in a host's main window has no frame of its own to speak of).
    fn chrome(window: &NSWindow, parent: &NSView) -> Option<(f64, f64)> {
        let frame = window.frame().size;
        let inner = parent.frame().size;
        let (w, h) = (frame.width - inner.width, frame.height - inner.height);
        (w >= 0.0 && h >= 0.0 && w < 200.0 && h < 300.0).then_some((w, h))
    }

    /// Whether this process is a view service (an AUv3 extension, Apple's AU hosting
    /// service): its windows only stand in for the host's.
    fn in_view_service() -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return true };
        let app = NSApplication::sharedApplication(mtm);
        let name = app.class().name().to_string_lossy().into_owned();
        name.contains("ViewService") || name.contains("Extension")
    }

    /// After the host resized a window of its own for a larger web view: move the window back
    /// onto its screen if the new size pushed it past an edge (a host keeps the top left
    /// corner; a full-screen size then hangs below the Dock). Only in-process, only a window
    /// that is the web view's own.
    pub fn keep_on_screen(view: &NSView) {
        if in_view_service() {
            return;
        }
        // SAFETY: AppKit calls on the main thread.
        let Some(parent) = (unsafe { view.superview() }) else { return };
        let Some(window) = view.window() else { return };
        if chrome(&window, &parent).is_none() {
            return;
        }
        let Some(screen) = window.screen() else { return };
        let vis = screen.visibleFrame();
        let mut f = window.frame();
        let x = f.origin.x.min(vis.origin.x + vis.size.width - f.size.width).max(vis.origin.x);
        let y = f.origin.y.max(vis.origin.y).min(vis.origin.y + vis.size.height - f.size.height);
        if (x, y) != (f.origin.x, f.origin.y) {
            f.origin = NSPoint::new(x, y);
            window.setFrameOrigin(f.origin);
        }
    }

    /// wry's own `set_bounds` needs the view to be in a window; this does not.
    pub fn set_frame(webview: &WebView, width: u32, height: u32) {
        let view = webview.webview();
        view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width as f64, height as f64)));
    }
}

fn page_url(path: &str) -> String {
    if cfg!(any(target_os = "windows", target_os = "android")) {
        format!("http://{SCHEME}.localhost/{path}")
    } else {
        format!("{SCHEME}://localhost/{path}")
    }
}

fn response(status: StatusCode, mime: &str, body: Cow<'static, [u8]>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, mime)
        .header("Cache-Control", "no-store")
        .body(body)
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(&[])))
}

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Whether and within which limits the web view may be resized (for the page's own resize).
#[derive(Debug, Clone, Copy)]
struct Sizing {
    resizable: bool,
    min: (u32, u32),
    max: (u32, u32),
}

fn serve(
    ctx: &ViewContext,
    sizing: &Sizing,
    assets: &'static [Asset],
    dev_dir: Option<&std::path::Path>,
    request: &Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path().trim_start_matches('/');
    let query = request.uri().query().unwrap_or("");
    let json = |body: String| response(StatusCode::OK, "application/json", Cow::Owned(body.into_bytes()));
    if let Some(route) = path.strip_prefix("__apf/plugin/").or((path == "__apf/plugin").then_some("")) {
        return plugin_request(ctx, request.method().as_str(), route, query, request.body());
    }
    match path {
        "__apf/apf.js" => return response(StatusCode::OK, mime_of(path), Cow::Borrowed(CLIENT_JS)),
        "__apf/info" => return json(info_json(ctx, sizing)),
        "__apf/values" => {
            let since = query_arg(query, "gen").and_then(|v| v.parse::<u64>().ok());
            let rev = ctx.controller().map(|c| c.revision());
            let since_rev = query_arg(query, "rev").and_then(|v| v.parse::<u64>().ok());
            if since == Some(ctx.generation()) && (rev.is_none() || since_rev == rev) {
                return response(StatusCode::NO_CONTENT, "application/json", Cow::Borrowed(&[]));
            }
            return json(values_json(ctx));
        }
        "__apf/echo" => {
            let body = request.body();
            let hash = body.iter().fold(0x811c_9dc5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x0100_0193));
            return json(format!("{{\"method\":\"{}\",\"length\":{},\"fnv1a\":{hash}}}", request.method().as_str(), body.len()));
        }
        "__apf/presets" => return json(presets_json(ctx)),
        "__apf/preset/load" => {
            let result = query_arg(query, "id").and_then(|id| Source::from_id(&id)).ok_or("no such preset".to_string());
            return json(outcome(result.and_then(|s| ctx.load_preset(&s))));
        }
        "__apf/preset/delete" => {
            let result = query_arg(query, "id").and_then(|id| Source::from_id(&id)).ok_or("no such preset".to_string());
            return json(outcome(result.and_then(|s| ctx.delete_preset(&s))));
        }
        "__apf/preset/save" => {
            let name = query_arg(query, "name").unwrap_or_default();
            let overwrite = query_arg(query, "overwrite").as_deref() == Some("1");
            let meta = save_metadata(query);
            let mut out = String::new();
            match ctx.save_preset_with(&name, overwrite, meta.as_ref()) {
                Ok(source) => {
                    out.push_str("{\"ok\":true,\"id\":");
                    json_str(&mut out, &source.id());
                    out.push('}');
                }
                Err(e) => {
                    let _ = write!(out, "{{\"ok\":false,\"exists\":{},\"error\":", matches!(e, SaveError::Exists(_)));
                    json_str(&mut out, &e.to_string());
                    out.push('}');
                }
            }
            return json(out);
        }
        "__apf/learn" => {
            let index = query_arg(query, "index").and_then(|v| v.parse::<i64>().ok()).unwrap_or(-1);
            ctx.learn(usize::try_from(index).ok());
            return json("{\"ok\":true}".into());
        }
        "__apf/forget" => {
            if let Some(index) = query_arg(query, "index").and_then(|v| v.parse::<usize>().ok()) {
                ctx.forget_cc(index);
            }
            return json("{\"ok\":true}".into());
        }
        _ => {}
    }
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(dir) = dev_dir {
        if !path.split('/').any(|seg| seg == "..") {
            if let Ok(bytes) = std::fs::read(dir.join(path)) {
                return response(StatusCode::OK, mime_of(path), Cow::Owned(bytes));
            }
        }
    }
    match assets.iter().find(|a| a.path == path) {
        Some(a) => response(StatusCode::OK, mime_of(path), Cow::Borrowed(a.bytes)),
        None => response(StatusCode::NOT_FOUND, "text/plain", Cow::Borrowed(b"not found")),
    }
}

/// `/__apf/plugin/<route>`: the plugin controller's own requests.
fn plugin_request(ctx: &ViewContext, method: &str, route: &str, query: &str, body: &[u8]) -> Response<Cow<'static, [u8]>> {
    let Some(controller) = ctx.controller() else {
        return response(StatusCode::NOT_FOUND, "text/plain", Cow::Borrowed(b"this plugin has no controller"));
    };
    let request = ControllerRequest { method, path: route, query, body };
    // A panic must not unwind into WebKit's callback (that aborts the host).
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| controller.request(&request))) {
        Ok(r) => {
            let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            response(status, &r.content_type, Cow::Owned(r.body))
        }
        Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, "text/plain", Cow::Borrowed(b"the plugin's request handler panicked")),
    }
}

fn handle_message(ctx: &ViewContext, sizing: &Sizing, page: &PageLink, msg: &str) {
    // Lines from the page for the host's log (apf.log): plugin hosts show no web console.
    if let Some(text) = msg.strip_prefix("log ") {
        eprintln!("[apf ui] {text}");
        return;
    }
    // "keys cmd+d backspace ...": the shortcuts the page takes before the host.
    if let Some(list) = msg.strip_prefix("keys") {
        *page.claims.borrow_mut() = keys::Claims::parse(list);
        return;
    }
    // "maximize": as large as the screen allows (the host decides, as for "resize").
    if msg == "maximize" {
        if sizing.resizable {
            #[cfg(target_os = "macos")]
            if let Some((w, h)) = page.view.borrow().as_ref().and_then(|v| v.load()).and_then(|v| mac::screen_room(&v)) {
                let w = (w.max(0.0) as u32).clamp(sizing.min.0, sizing.max.0);
                let h = (h.max(0.0) as u32).clamp(sizing.min.1, sizing.max.1);
                ctx.request_resize(w, h);
            }
            #[cfg(target_os = "windows")]
            if let Some((w, h)) = win::screen_room(page.parent.get(), page.scale.get()) {
                let w = (w.max(0.0) as u32).clamp(sizing.min.0, sizing.max.0);
                let h = (h.max(0.0) as u32).clamp(sizing.min.1, sizing.max.1);
                ctx.request_resize(w, h);
            }
        }
        return;
    }
    // "hostkey down|up <key code> <alt>": a key the page did not take, for the host (Windows).
    if let Some(rest) = msg.strip_prefix("hostkey ") {
        #[cfg(target_os = "windows")]
        {
            let mut it = rest.split_ascii_whitespace();
            if let (Some(dir), Some(Ok(code)), Some(alt)) = (it.next(), it.next().map(str::parse::<u32>), it.next()) {
                win::forward_key(page.parent.get(), dir == "down", code, alt == "1");
            }
        }
        let _ = rest;
        return;
    }
    let _ = page;
    // "resize w h": the page's own resize grip. The host decides; it answers with set_size.
    if let Some(rest) = msg.strip_prefix("resize ") {
        let mut wh = rest.split_ascii_whitespace().filter_map(|v| v.parse::<f64>().ok());
        if let (true, Some(w), Some(h)) = (sizing.resizable, wh.next(), wh.next()) {
            let w = (w.max(0.0) as u32).clamp(sizing.min.0, sizing.max.0);
            let h = (h.max(0.0) as u32).clamp(sizing.min.1, sizing.max.1);
            ctx.request_resize(w, h);
        }
        return;
    }
    let mut parts = msg.split_ascii_whitespace();
    let verb = parts.next();
    let Some(index) = parts.next().and_then(|i| i.parse::<usize>().ok()) else { return };
    match verb {
        Some("begin") => ctx.begin_edit(index),
        Some("end") => ctx.end_edit(index),
        Some("set") => {
            if let Some(v) = parts.next().and_then(|v| v.parse::<f64>().ok()) {
                ctx.set_normalized(index, v);
            }
        }
        _ => {}
    }
}

fn json_str(out: &mut String, s: &str) {
    apf::json::write_str(out, s)
}

fn outcome(result: Result<(), String>) -> String {
    match result {
        Ok(()) => "{\"ok\":true}".into(),
        Err(e) => {
            let mut out = String::from("{\"ok\":false,\"error\":");
            json_str(&mut out, &e);
            out.push('}');
            out
        }
    }
}

/// The metadata of a save request, `None` when it names none of it.
fn save_metadata(query: &str) -> Option<Metadata> {
    let arg = |key: &str| query_arg(query, key);
    let (category, subcategory, mood, description, author) = (arg("type"), arg("subtype"), arg("mood"), arg("description"), arg("author"));
    if [&category, &subcategory, &mood, &description, &author].iter().all(|a| a.is_none()) {
        return None;
    }
    let meta = Metadata {
        category: category.unwrap_or_default(),
        subcategory: subcategory.unwrap_or_default(),
        tags: mood.map(|m| m.split(',').map(str::to_owned).collect()).unwrap_or_default(),
        description: description.unwrap_or_default(),
        author: author.unwrap_or_default(),
    };
    Some(meta.cleaned())
}

fn presets_json(ctx: &ViewContext) -> String {
    let presets = ctx.presets();
    let mut out = String::with_capacity(64 + 200 * presets.len());
    out.push_str("{\"presets\":[");
    for (i, p) in presets.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        json_str(&mut out, &p.source.id());
        out.push_str(",\"name\":");
        json_str(&mut out, &p.name);
        let _ = write!(out, ",\"factory\":{},\"type\":", p.is_factory());
        json_str(&mut out, &p.meta.category);
        out.push_str(",\"subtype\":");
        json_str(&mut out, &p.meta.subcategory);
        out.push_str(",\"mood\":[");
        for (j, t) in p.meta.tags.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            json_str(&mut out, t);
        }
        out.push_str("],\"description\":");
        json_str(&mut out, &p.meta.description);
        out.push_str(",\"author\":");
        json_str(&mut out, &p.meta.author);
        out.push('}');
    }
    out.push_str("],\"dir\":");
    json_str(&mut out, &ctx.user_preset_dir().map(|d| d.display().to_string()).unwrap_or_default());
    out.push('}');
    out
}

/// The current preset and the MIDI learn state, as members of the values object.
fn session_json(ctx: &ViewContext, out: &mut String) {
    out.push_str(",\"preset\":");
    match ctx.current_preset() {
        Some((source, name, changed)) => {
            out.push_str("{\"id\":");
            json_str(out, &source.id());
            out.push_str(",\"name\":");
            json_str(out, &name);
            let _ = write!(out, ",\"factory\":{},\"changed\":{changed}}}", matches!(source, Source::Factory(_)));
        }
        None => out.push_str("null"),
    }
    let _ = write!(out, ",\"learn\":{},\"cc\":[", ctx.learning().map_or(-1, |i| i as i64));
    for (i, (cc, index)) in ctx.cc_mappings().into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "[{cc},{index}]");
    }
    out.push(']');
    let _ = write!(
        out,
        ",\"controllers\":{{\"wheel\":{},\"pedal\":{},\"bend\":{}}}",
        ctx.cc_value(apf::cc::MOD_WHEEL) as f32 / 127.0,
        ctx.cc_value(apf::cc::SUSTAIN_PEDAL) >= 64,
        ctx.pitch_bend()
    );
}

fn info_json(ctx: &ViewContext, sizing: &Sizing) -> String {
    let params = ctx.params();
    let mut out = String::with_capacity(256 * params.len());
    let _ = write!(out, "{{\"gen\":{},", ctx.generation());
    if sizing.resizable {
        let _ = write!(out, "\"resize\":{{\"min\":[{},{}],\"max\":[{},{}]}},", sizing.min.0, sizing.min.1, sizing.max.0, sizing.max.1);
    }
    if let Some(c) = ctx.controller() {
        let _ = write!(out, "\"rev\":{},", c.revision());
    }
    out.push_str("\"params\":[");
    for (i, def) in params.defs().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let kind = match def.kind {
            ParamKind::Float { .. } => "float",
            ParamKind::Int { .. } => "int",
            ParamKind::Bool { .. } => "bool",
            ParamKind::Choice { .. } => "choice",
        };
        let _ = write!(out, "{{\"index\":{i},\"key\":");
        json_str(&mut out, def.key);
        out.push_str(",\"name\":");
        json_str(&mut out, def.name);
        out.push_str(",\"unit\":");
        json_str(&mut out, def.unit);
        let _ = write!(out, ",\"kind\":\"{kind}\",\"steps\":{}", def.step_count().unwrap_or(0));
        let _ = write!(out, ",\"default\":{}", def.host_to_normalized(def.default_host_value()));
        if let ParamKind::Choice { options, .. } = def.kind {
            out.push_str(",\"options\":[");
            for (j, o) in options.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                json_str(&mut out, o);
            }
            out.push(']');
        }
        let _ = write!(out, ",\"value\":{},\"text\":", params.normalized(i));
        let mut text = String::new();
        params.text(i, &mut text);
        json_str(&mut out, &text);
        out.push('}');
    }
    out.push(']');
    session_json(ctx, &mut out);
    out.push('}');
    out
}

fn values_json(ctx: &ViewContext) -> String {
    let params = ctx.params();
    let mut out = String::with_capacity(32 * params.len());
    // Read the generation first: a change racing with this read shows up in the next poll.
    let _ = write!(out, "{{\"gen\":{},", ctx.generation());
    if let Some(c) = ctx.controller() {
        let _ = write!(out, "\"rev\":{},", c.revision());
    }
    out.push_str("\"values\":[");
    let mut text = String::new();
    for i in 0..params.len() {
        if i > 0 {
            out.push(',');
        }
        text.clear();
        params.text(i, &mut text);
        let _ = write!(out, "[{},", params.normalized(i));
        json_str(&mut out, &text);
        out.push(']');
    }
    out.push(']');
    session_json(ctx, &mut out);
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use apf::prelude::*;
    use std::sync::{Arc, Mutex};

    struct Notes(Mutex<String>);

    impl Controller for Notes {
        fn save(&self) -> String {
            self.0.lock().unwrap().clone()
        }
        fn load(&self, text: &str) -> Result<(), String> {
            *self.0.lock().unwrap() = text.into();
            Ok(())
        }
        fn reset(&self) {
            self.0.lock().unwrap().clear();
        }
        fn revision(&self) -> u64 {
            self.0.lock().unwrap().len() as u64
        }
        fn request(&self, r: &ControllerRequest) -> ControllerResponse {
            match (r.method, r.path) {
                ("GET", "notes") => ControllerResponse::json(format!("{{\"notes\":{:?},\"q\":{:?}}}", self.save(), r.query_arg("q").unwrap_or_default())),
                ("POST", "notes") => {
                    let _ = self.load(r.body_text().unwrap_or(""));
                    ControllerResponse::json("{\"ok\":true}")
                }
                ("GET", "bytes") => ControllerResponse::bytes(vec![0, 1, 2, 255]),
                ("GET", "teapot") => ControllerResponse::new(418, "text/plain", b"no".to_vec()),
                _ => ControllerResponse::not_found(),
            }
        }
    }

    struct WithController(bool);

    impl Plugin for WithController {
        const INFO: PluginInfo =
            PluginInfo { id: "dev.test.webview", name: "Webview", vendor: "Test", url: "", version: "1", description: "", features: &[] };
        const LAYOUT: AudioLayout = AudioLayout::INSTRUMENT_STEREO;
        fn new() -> Self {
            Self(true)
        }
        fn process(&mut self, _: &mut AudioBuffer, _: &ProcessContext) -> ProcessStatus {
            ProcessStatus::Sleep
        }
        fn controller(&mut self, _host: HostHandle) -> Option<Arc<dyn Controller>> {
            self.0.then(|| Arc::new(Notes(Mutex::new("C E G".into()))) as Arc<dyn Controller>)
        }
    }

    fn call(ctx: &ViewContext, method: &str, uri: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
        let request = Request::builder().method(method).uri(format!("apf://localhost{uri}")).body(body.to_vec()).unwrap();
        let sizing = Sizing { resizable: true, min: (300, 200), max: (2000, 1500) };
        let r = serve(ctx, &sizing, &[], None, &request);
        let mime = r.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
        (r.status().as_u16(), mime, r.body().to_vec())
    }

    #[test]
    fn plugin_requests_reach_the_controller() {
        let ctx = ViewContext::without_host(&mut WithController(true));
        let (status, mime, body) = call(&ctx, "GET", "/__apf/plugin/notes?q=a%20b", b"");
        assert_eq!((status, mime.as_str()), (200, "application/json"));
        assert_eq!(String::from_utf8(body).unwrap(), r#"{"notes":"C E G","q":"a b"}"#);
        // A POST body arrives whole, also non-ASCII.
        let (status, _, _) = call(&ctx, "POST", "/__apf/plugin/notes", "D F# A — é".as_bytes());
        assert_eq!(status, 200);
        assert_eq!(ctx.controller().unwrap().save(), "D F# A — é");
        assert_eq!(call(&ctx, "GET", "/__apf/plugin/bytes", b"").2, [0, 1, 2, 255]);
        assert_eq!(call(&ctx, "GET", "/__apf/plugin/teapot", b"").0, 418);
        assert_eq!(call(&ctx, "GET", "/__apf/plugin/nothing", b"").0, 404);
        // The revision rides along with the values, and a change of it alone is news.
        let (_, _, values) = call(&ctx, "GET", "/__apf/values", b"");
        let values = String::from_utf8(values).unwrap();
        let rev = ctx.controller().unwrap().revision();
        assert!(values.contains(&format!("\"rev\":{rev},")), "{values}");
        let gen = ctx.generation();
        assert_eq!(call(&ctx, "GET", &format!("/__apf/values?gen={gen}&rev={rev}"), b"").0, 204);
        call(&ctx, "POST", "/__apf/plugin/notes", b"C");
        assert_eq!(call(&ctx, "GET", &format!("/__apf/values?gen={gen}&rev={rev}"), b"").0, 200);
    }

    /// The tests that save presets use a home folder of their own (set once, before any of
    /// them reads it), so the user's presets are never touched.
    fn test_home() -> &'static std::path::Path {
        static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        HOME.get_or_init(|| {
            let home = std::env::temp_dir().join(format!("apf-webview-home-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            std::fs::create_dir_all(&home).unwrap();
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
            std::env::remove_var("XDG_DATA_HOME");
            home
        })
    }

    /// A factory preset of about 6 KB, like a modular patch with its metadata.
    fn big_preset(i: usize) -> String {
        let types = [("Bass", "Sub"), ("Lead", "Saw"), ("Pad", "Warm"), ("Drums", "Kick"), ("FX", "Riser")];
        let (ty, sub) = types[i % types.len()];
        let modules: Vec<String> = (0..70)
            .map(|m| format!(r#"{{"id":{m},"type":"vco","x":{},"params":{{"freq":0.{i:04}{m:02},"shape":0.25,"level":0.8}}}}"#, m * 10))
            .collect();
        format!(
            r#"{{"apf-preset": 1, "plugin": "dev.test.webview", "name": "Sound {i:03}", "type": "{ty}", "subtype": "{sub}",
               "mood": ["Dark", "Warm"], "description": "Preset number {i} of a generated library", "params": {{}},
               "controller": {{"rev": 1, "modules": [{}], "cables": []}}}}"#,
            modules.join(",")
        )
    }

    fn median_ms(mut f: impl FnMut()) -> f64 {
        let mut times: Vec<f64> = (0..15)
            .map(|_| {
                let t = std::time::Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(f64::total_cmp);
        times[times.len() / 2]
    }

    #[test]
    fn a_large_preset_library_lists_fast_with_its_metadata() {
        let home = test_home();
        let texts: Vec<&'static str> = (0..250).map(|i| &*big_preset(i).leak()).collect();
        let size = texts.iter().map(|t| t.len()).sum::<usize>() / texts.len();
        assert!((5000..8000).contains(&size), "{size} bytes a preset");
        let texts: &'static [&'static str] = texts.leak();

        let t = std::time::Instant::now();
        let ctx = ViewContext::without_host_with_presets(&mut WithController(true), texts);
        let create_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = std::time::Instant::now();
        let (status, mime, body) = call(&ctx, "GET", "/__apf/presets", b"");
        let first_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!((status, mime.as_str()), (200, "application/json"));
        let body = String::from_utf8(body).unwrap();
        let list = apf::json::parse(&body).unwrap();
        let Some(apf::json::Value::Arr(presets)) = list.get("presets") else { panic!("{body}") };
        assert_eq!(presets.len(), 250);
        let first = &presets[0];
        assert_eq!(first.get("name").and_then(|v| v.as_str()), Some("Sound 000"));
        assert_eq!(first.get("id").and_then(|v| v.as_str()), Some("factory:Sound 000"));
        assert_eq!(first.get("type").and_then(|v| v.as_str()), Some("Bass"));
        assert_eq!(first.get("subtype").and_then(|v| v.as_str()), Some("Sub"));
        assert_eq!(first.get("description").and_then(|v| v.as_str()), Some("Preset number 0 of a generated library"));
        assert!(body.contains(r#""mood":["Dark","Warm"]"#), "{}", &body[..400]);
        assert_eq!(presets[249].get("name").and_then(|v| v.as_str()), Some("Sound 249"), "the plugin's order");
        let warm_ms = median_ms(|| {
            call(&ctx, "GET", "/__apf/presets", b"");
        });

        // ... and as many user presets beside them: read once, then only when changed.
        let dir = ctx.user_preset_dir().unwrap();
        assert!(dir.starts_with(home));
        std::fs::create_dir_all(dir.join("Mine")).unwrap();
        for (i, t) in texts.iter().enumerate() {
            std::fs::write(dir.join(format!("Mine/User {i:03}.apfpreset")), t.replace("Sound ", "User ")).unwrap();
        }
        let t = std::time::Instant::now();
        let listed = ctx.presets();
        let user_first_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(listed.iter().filter(|p| !p.is_factory() && p.name.starts_with("User ")).count(), 250);
        let user_warm_ms = median_ms(|| {
            call(&ctx, "GET", "/__apf/presets", b"");
        });
        std::fs::remove_dir_all(dir.join("Mine")).unwrap();
        eprintln!(
            "250 factory presets of {size} bytes: instance {create_ms:.1} ms, /__apf/presets first {first_ms:.2} ms, then {warm_ms:.2} ms; \
             with 250 user presets: first {user_first_ms:.1} ms, then {user_warm_ms:.2} ms"
        );
        assert!(warm_ms < 20.0, "{warm_ms} ms");
        assert!(user_warm_ms < 20.0, "{user_warm_ms} ms");
    }

    #[test]
    fn saving_a_preset_carries_its_metadata() {
        let _home = test_home();
        let ctx = ViewContext::without_host(&mut WithController(true));
        let save = |q: &str| String::from_utf8(call(&ctx, "GET", &format!("/__apf/preset/save?{q}"), b"").2).unwrap();
        let r = save("name=My%20Sub&type=Bass&subtype=Sub&mood=Dark,%20Deep,,dark&description=Low%20and%20round&author=Me");
        assert!(r.starts_with(r#"{"ok":true,"id":"file:"#), "{r}");
        let list = String::from_utf8(call(&ctx, "GET", "/__apf/presets", b"").2).unwrap();
        assert!(
            list.contains(r#""name":"My Sub","factory":false,"type":"Bass","subtype":"Sub","mood":["Dark","Deep"],"description":"Low and round","author":"Me"}"#),
            "{list}"
        );
        // The file says it too, in the format's own keys.
        let dir = ctx.user_preset_dir().unwrap();
        let text = std::fs::read_to_string(dir.join("My Sub.apfpreset")).unwrap();
        assert!(text.contains("\"type\": \"Bass\",\n  \"subtype\": \"Sub\",\n  \"mood\": [\"Dark\", \"Deep\"]"), "{text}");
        // Overwritten without metadata: kept. With some: replaced (missing members empty).
        assert_eq!(save("name=My%20Sub"), r#"{"ok":false,"exists":true,"error":"a preset of that name exists"}"#);
        assert!(save("name=My%20Sub&overwrite=1").contains("\"ok\":true"));
        let p = ctx.presets().into_iter().find(|p| p.name == "My Sub").unwrap();
        assert_eq!((p.meta.category.as_str(), p.meta.tags.len()), ("Bass", 2));
        assert!(save("name=My%20Sub&overwrite=1&mood=Warm").contains("\"ok\":true"));
        let p = ctx.presets().into_iter().find(|p| p.name == "My Sub").unwrap();
        assert_eq!(p.meta, Metadata { tags: vec!["Warm".into()], ..Default::default() });
        // A preset without any metadata lists empty members.
        assert!(save("name=Plain").contains("\"ok\":true"));
        let list = String::from_utf8(call(&ctx, "GET", "/__apf/presets", b"").2).unwrap();
        assert!(list.contains(r#""name":"Plain","factory":false,"type":"","subtype":"","mood":[],"description":"","author":""}"#), "{list}");
        for name in ["My Sub", "Plain"] {
            std::fs::remove_file(dir.join(format!("{name}.apfpreset"))).unwrap();
        }
    }

    #[test]
    fn without_a_controller_plugin_requests_are_not_found() {
        let ctx = ViewContext::without_host(&mut WithController(false));
        assert!(ctx.controller().is_none());
        assert_eq!(call(&ctx, "GET", "/__apf/plugin/notes", b"").0, 404);
        assert_eq!(call(&ctx, "POST", "/__apf/plugin/", b"{}").0, 404);
        let gen = ctx.generation();
        assert_eq!(call(&ctx, "GET", &format!("/__apf/values?gen={gen}"), b"").0, 204);
        // A resizable web view tells the page its limits (for apf.resize and the grip).
        let info = String::from_utf8(call(&ctx, "GET", "/__apf/info", b"").2).unwrap();
        assert!(info.contains(r#""resize":{"min":[300,200],"max":[2000,1500]}"#), "{info}");
        let (status, _, body) = call(&ctx, "POST", "/__apf/echo", b"abc");
        assert_eq!(status, 200);
        assert_eq!(String::from_utf8(body).unwrap(), format!("{{\"method\":\"POST\",\"length\":3,\"fnv1a\":{}}}", 0x1a47e90bu32));
    }
}
