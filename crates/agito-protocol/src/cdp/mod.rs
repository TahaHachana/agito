//! Generated Chrome DevTools Protocol bindings. Do not edit by hand;
//! run `cargo xtask codegen` instead.

/// **Experimental.**
pub mod accessibility;
/// A domain for ad-related metrics and data.
///
/// **Experimental.**
pub mod ads;
/// **Experimental.**
pub mod animation;
/// Audits domain allows investigation of page violations and possible improvements.
///
/// **Experimental.**
pub mod audits;
/// Defines commands and events for Autofill.
///
/// **Experimental.**
pub mod autofill;
/// Defines events for background web platform features.
///
/// **Experimental.**
pub mod background_service;
/// This domain allows configuring virtual Bluetooth devices to test
/// the web-bluetooth API.
///
/// **Experimental.**
pub mod bluetooth_emulation;
/// The Browser domain defines methods and events for browser managing.
pub mod browser;
/// **Experimental.**
pub mod cache_storage;
/// A domain for interacting with Cast, Presentation API, and Remote Playback API
/// functionalities.
///
/// **Experimental.**
pub mod cast;
/// This domain is deprecated - use Runtime or Log instead.
///
/// **Deprecated.**
pub mod console;
/// This domain exposes the current state of the CrashReportContext API.
///
/// **Experimental.**
pub mod crash_report_context;
/// This domain exposes CSS read/write operations. All CSS objects (stylesheets, rules, and styles)
/// have an associated `id` used in subsequent operations on the related object. Each object type has
/// a specific `id` structure, and those are not interchangeable between objects of different kinds.
/// CSS objects can be loaded using the `get*ForNode()` calls (which accept a DOM node id). A client
/// can also keep track of stylesheets via the `styleSheetAdded`/`styleSheetRemoved` events and
/// subsequently load the required stylesheet contents using the `getStyleSheet[Text]()` methods.
///
/// **Experimental.**
pub mod css;
/// Debugger domain exposes JavaScript debugging capabilities. It allows setting and removing
/// breakpoints, stepping through execution, exploring stack traces, etc.
pub mod debugger;
/// **Experimental.**
pub mod device_access;
/// **Experimental.**
pub mod device_orientation;
/// This domain allows interacting with the Digital Credentials API for automation.
///
/// **Experimental.**
pub mod digital_credentials;
/// This domain exposes DOM read/write operations. Each DOM Node is represented with its mirror object
/// that has an `id`. This `id` can be used to get additional information on the Node, resolve it into
/// the JavaScript object wrapper, etc. It is important that client receives DOM events only for the
/// nodes that are known to the client. Backend keeps track of the nodes that were sent to the client
/// and never sends the same node twice. It is client's responsibility to collect information about
/// the nodes that were sent to the client. Note that `iframe` owner elements will return
/// corresponding document elements as their child nodes.
pub mod dom;
/// DOM debugging allows setting breakpoints on particular DOM operations and events. JavaScript
/// execution will stop on these operations as if there was a regular breakpoint set.
pub mod dom_debugger;
/// This domain facilitates obtaining document snapshots with DOM, layout, and style information.
///
/// **Experimental.**
pub mod dom_snapshot;
/// Query and modify DOM storage.
///
/// **Experimental.**
pub mod dom_storage;
/// This domain emulates different environments for the page.
pub mod emulation;
/// EventBreakpoints permits setting JavaScript breakpoints on operations and events
/// occurring in native code invoked from JavaScript. Once breakpoint is hit, it is
/// reported through Debugger domain, similarly to regular breakpoints being hit.
///
/// **Experimental.**
pub mod event_breakpoints;
/// Defines commands and events for browser extensions.
///
/// **Experimental.**
pub mod extensions;
/// This domain allows interacting with the FedCM dialog.
///
/// **Experimental.**
pub mod fed_cm;
/// A domain for letting clients substitute browser's network layer with client code.
pub mod fetch;
/// **Experimental.**
pub mod file_system;
/// This domain provides commands to trigger the "Find in page" feature.
///
/// **Experimental.**
pub mod find_in_page;
/// This domain provides experimental commands only supported in headless mode.
///
/// **Experimental.**
pub mod headless_experimental;
/// **Experimental.**
pub mod heap_profiler;
/// **Experimental.**
pub mod indexed_db;
pub mod input;
/// **Experimental.**
pub mod inspector;
/// Input/Output operations for streams produced by DevTools.
pub mod io;
/// **Experimental.**
pub mod layer_tree;
/// Provides access to log entries.
pub mod log;
/// This domain allows detailed inspection of media elements.
///
/// **Experimental.**
pub mod media;
/// **Experimental.**
pub mod memory;
/// Network domain allows tracking network activities of the page. It exposes information about http,
/// file, data and other requests and responses, their headers, bodies, timing, etc.
pub mod network;
/// This domain provides various functionality related to drawing atop the inspected page.
///
/// **Experimental.**
pub mod overlay;
/// Actions and events related to the inspected page belong to the page domain.
pub mod page;
pub mod performance;
/// Reporting of performance timeline events, as specified in
/// https://w3c.github.io/performance-timeline/#dom-performanceobserver.
///
/// **Experimental.**
pub mod performance_timeline;
/// **Experimental.**
pub mod preload;
pub mod profiler;
/// This domain allows interacting with the browser to control PWAs.
///
/// **Experimental.**
pub mod pwa;
/// Runtime domain exposes JavaScript runtime by means of remote evaluation and mirror objects.
/// Evaluation results are returned as mirror object that expose object type, string representation
/// and unique identifier that can be used for further object reference. Original objects are
/// maintained in memory unless they are either explicitly released or are released along with the
/// other objects in their object group.
pub mod runtime;
/// This domain is deprecated.
///
/// **Deprecated.**
pub mod schema;
pub mod security;
/// **Experimental.**
pub mod service_worker;
/// **Experimental.**
pub mod smart_card_emulation;
/// **Experimental.**
pub mod storage;
/// The SystemInfo domain defines methods and events for querying low-level system information.
///
/// **Experimental.**
pub mod system_info;
/// Supports additional targets discovery and allows to attach to them.
pub mod target;
/// The Tethering domain defines methods and events for browser port binding.
///
/// **Experimental.**
pub mod tethering;
pub mod tracing;
/// This domain allows inspection of Web Audio API.
/// https://webaudio.github.io/web-audio-api/
///
/// **Experimental.**
pub mod web_audio;
/// This domain allows configuring virtual authenticators to test the WebAuthn
/// API.
///
/// **Experimental.**
pub mod web_authn;
/// **Experimental.**
pub mod web_mcp;
