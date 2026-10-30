//! The desktop interface of Ferrufus, an unofficial Linux port of Rufus
//! (not affiliated with or endorsed by the Rufus project).
//!
//! Laid out like the Rufus main window, minus everything that writes: this
//! build lists devices, inspects images, computes checksums and reviews
//! dry-run plans. It has no Start button, never selects a device by itself
//! and refuses to run as root.

#![forbid(unsafe_code)]

use eframe::egui::{self, Color32, RichText};
use ferrufus_core::cancel::CancelToken;
use ferrufus_core::device::policy::ListingOptions;
use ferrufus_core::device::sysfs::SysRoot;
use ferrufus_core::device::{DeviceEntry, DeviceScan, ScanOptions, scan};
use ferrufus_core::hash::{HashAlgorithm, HashReport, hash_file};
use ferrufus_core::image::{ImageReport, InspectOptions, inspect};
use ferrufus_core::options::{Choices, DeviceInput, FormatOptions, format_options};
use ferrufus_core::plan::{
    CheckStatus, PlanContext, PlanOptions, PrepareRequest, WriteMode, prepare,
};
use ferrufus_core::units::{SizeUnits, size_to_human_readable};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// The banner shown at all times.
pub const BANNER: &str = "Read-only preview: this build cannot write to devices.";
/// The text of the device list when nothing is selected.
pub const NO_DEVICE: &str = "(select a device)";
/// How often the device list is refreshed, for hotplug.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Whether `/proc/self/status` content describes a process running as root
/// (real or effective user ID 0).
pub fn running_as_root(status: &str) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .is_some_and(|ids| ids.split_whitespace().take(2).any(|id| id == "0"))
}

/// Results of background work.
#[derive(Debug)]
enum Message {
    Scan(Result<DeviceScan, String>),
    Image(Box<Result<ImageReport, String>>),
    Hash(Result<HashReport, String>),
    Plan(Box<Result<PlanContext, String>>),
    Picked(Option<PathBuf>),
}

/// Startup options.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// System tree to read devices from (`/` normally).
    pub sysroot: Option<PathBuf>,
    /// Image to preselect (`-i`/`--iso`).
    pub image: Option<PathBuf>,
    /// Listing options (`-x` enables USB hard drives).
    pub listing: ListingOptions,
    /// Run work on the UI thread (for tests).
    pub synchronous: bool,
}

/// The application state.
#[derive(Debug)]
pub struct FerrufusApp {
    root: SysRoot,
    options: Options,
    listing: ListingOptions,
    scan: Option<DeviceScan>,
    scan_error: Option<String>,
    /// Stable id of the device the user picked.
    selected: Option<String>,
    image_path: String,
    image: Option<Result<ImageReport, String>>,
    hashes: Option<Result<HashReport, String>>,
    plan: Option<Result<PlanContext, String>>,
    /// Format choices the user made.
    choices: Choices,
    /// The write mode the user picked (automatic otherwise).
    mode: Option<WriteMode>,
    log: Vec<String>,
    busy: usize,
    last_scan: Option<Instant>,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
}

impl FerrufusApp {
    /// Creates the application and starts the first device scan.
    pub fn new(options: Options) -> Self {
        let (sender, receiver) = channel();
        let root = options
            .sysroot
            .as_ref()
            .map_or_else(SysRoot::host, SysRoot::new);
        let mut app = Self {
            root,
            listing: options.listing.clone(),
            options,
            scan: None,
            scan_error: None,
            selected: None,
            image_path: String::new(),
            image: None,
            hashes: None,
            plan: None,
            choices: Choices::default(),
            mode: None,
            log: vec!["Ferrufus started (read-only preview).".to_string()],
            busy: 0,
            last_scan: None,
            sender,
            receiver,
        };
        app.refresh();
        if let Some(image) = app.options.image.clone() {
            app.set_image(image);
        }
        app
    }

    /// The selected device, if it is still offered.
    pub fn selected_device(&self) -> Option<&DeviceEntry> {
        let id = self.selected.as_deref()?;
        self.scan.as_ref()?.listed().find(|d| d.stable_id == id)
    }

    /// The device scan.
    pub fn scan(&self) -> Option<&DeviceScan> {
        self.scan.as_ref()
    }

    /// The log lines.
    pub fn log(&self) -> &[String] {
        &self.log
    }

    /// Selects a listed device by stable id (as clicking it does).
    pub fn select(&mut self, stable_id: &str) -> bool {
        let listed = self
            .scan
            .as_ref()
            .is_some_and(|s| s.listed().any(|d| d.stable_id == stable_id));
        if listed {
            self.selected = Some(stable_id.to_string());
            self.plan = None;
        }
        listed
    }

    fn spawn(
        &mut self,
        job: impl FnOnce() -> Message + Send + 'static,
        ctx: Option<&egui::Context>,
    ) {
        if self.options.synchronous {
            let message = job();
            self.handle(message);
            return;
        }
        self.busy = self.busy.saturating_add(1);
        let sender = self.sender.clone();
        let ctx = ctx.cloned();
        std::thread::spawn(move || {
            let _ = sender.send(job());
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn refresh(&mut self) {
        self.refresh_with(None);
    }

    fn refresh_with(&mut self, ctx: Option<&egui::Context>) {
        self.last_scan = Some(Instant::now());
        let root = self.root.clone();
        let mut options = ScanOptions::for_this_process(self.listing.clone());
        if self.options.sysroot.is_some() {
            options.program_paths.clear();
        }
        self.spawn(
            move || Message::Scan(scan(&root, &options).map_err(|e| e.to_string())),
            ctx,
        );
    }

    fn set_image(&mut self, path: PathBuf) {
        self.image_path = path.display().to_string();
        self.image = None;
        self.hashes = None;
        self.plan = None;
        // A new image gets a new proposed label and default mode (Rufus
        // resets user_changed_label).
        self.choices.label = None;
        self.mode = None;
        self.spawn(
            move || {
                Message::Image(Box::new(
                    inspect(&path, &InspectOptions::default()).map_err(|e| e.to_string()),
                ))
            },
            None,
        );
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::Scan(result) => {
                match result {
                    Ok(scan) => {
                        self.scan_error = None;
                        self.scan = Some(scan);
                    }
                    Err(e) => self.scan_error = Some(e),
                }
                // Keep the selection only if that very device is still
                // offered; never pick another one.
                if self.selected.is_some() && self.selected_device().is_none() {
                    self.log.push(
                        "The selected device is gone or no longer eligible; selection cleared."
                            .into(),
                    );
                    self.selected = None;
                    self.plan = None;
                }
            }
            Message::Image(result) => {
                let result = *result;
                match &result {
                    Ok(report) => self
                        .log
                        .push(format!("Image analyzed: {:?}", report.classification)),
                    Err(e) => self.log.push(format!("Cannot analyze the image: {e}")),
                }
                self.image = Some(result);
            }
            Message::Hash(result) => self.hashes = Some(result),
            Message::Plan(result) => {
                let result = *result;
                if let Ok(context) = &result {
                    let verdict = if context.plan.blocked {
                        "blocked"
                    } else {
                        "possible (not executable)"
                    };
                    self.log.push(format!(
                        "Dry-run plan {verdict}: {}",
                        context.plan.fingerprint
                    ));
                }
                self.plan = Some(result);
            }
            Message::Picked(Some(path)) => self.set_image(path),
            Message::Picked(None) => {}
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            self.busy = self.busy.saturating_sub(1);
            self.handle(message);
        }
        if !self.options.synchronous {
            if self
                .last_scan
                .is_none_or(|t| t.elapsed() >= REFRESH_INTERVAL)
            {
                self.refresh_with(Some(ctx));
            }
            ctx.request_repaint_after(REFRESH_INTERVAL);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            self.set_image(path);
        }
    }

    /// Draws the window contents.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.poll(&ui.ctx().clone());
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label(
                RichText::new(BANNER)
                    .strong()
                    .color(Color32::from_rgb(0xb0, 0x40, 0x00)),
            );
            ui.separator();
            self.device_section(ui);
            ui.separator();
            self.image_section(ui);
            ui.separator();
            self.format_section(ui);
            ui.separator();
            self.plan_section(ui);
            ui.separator();
            ui.heading("Log");
            for line in self.log.iter().rev().take(8) {
                ui.label(line);
            }
        });
    }

    fn device_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Drive Properties");
        let selected_text = self
            .selected_device()
            .map_or(NO_DEVICE.to_string(), |d| d.display_name.clone());
        let mut choice: Option<String> = None;
        ui.horizontal(|ui| {
            egui::ComboBox::new("device", "Device")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    if let Some(scan) = &self.scan {
                        for device in scan.listed() {
                            let picked =
                                self.selected.as_deref() == Some(device.stable_id.as_str());
                            if ui.selectable_label(picked, &device.display_name).clicked() {
                                choice = Some(device.stable_id.clone());
                            }
                        }
                    }
                });
            if ui.button("Refresh").clicked() {
                self.refresh_with(Some(ui.ctx()));
            }
        });
        if let Some(id) = choice {
            self.select(&id);
        }
        match (&self.scan, &self.scan_error) {
            (_, Some(error)) => {
                ui.colored_label(Color32::RED, format!("Cannot list devices: {error}"));
            }
            (Some(scan), None) => {
                let count = scan.listed().count();
                let excluded = scan.devices.len().saturating_sub(count);
                ui.label(format!("{count} device(s) found, {excluded} excluded."));
                for unresolved in &scan.system.unresolved {
                    ui.colored_label(Color32::RED, format!("Warning: {unresolved}"));
                }
            }
            (None, None) => {
                ui.label("Scanning…");
            }
        }
        let before = self.listing.clone();
        egui::CollapsingHeader::new("Show advanced drive properties").show(ui, |ui| {
            ui.checkbox(&mut self.listing.list_usb_hdd, "List USB Hard Drives");
            ui.checkbox(
                &mut self.listing.list_non_usb_removable,
                "List non-USB removable drives",
            );
            ui.checkbox(&mut self.listing.list_virtual, "List loop devices");
            if let Some(scan) = &self.scan {
                for device in scan.devices.iter().filter(|d| !d.decision.listed) {
                    let reasons: Vec<&str> = device
                        .decision
                        .reasons
                        .iter()
                        .map(|r| r.message.as_str())
                        .collect();
                    ui.label(format!(
                        "Excluded {}: {}",
                        device.facts.devnode,
                        reasons.join("; ")
                    ));
                }
            }
        });
        if self.listing != before {
            self.refresh_with(Some(ui.ctx()));
        }
    }

    fn image_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Boot selection");
        let mut submitted = false;
        ui.horizontal(|ui| {
            let label = ui.label("Image");
            let edit = ui
                .text_edit_singleline(&mut self.image_path)
                .labelled_by(label.id);
            submitted = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("SELECT").clicked() {
                self.spawn(
                    || {
                        Message::Picked(
                            rfd::FileDialog::new()
                                .add_filter(
                                    "Disk images",
                                    &[
                                        "iso", "img", "bin", "raw", "vhd", "vhdx", "gz", "xz",
                                        "zst", "bz2", "zip", "wim", "ffu", "vtsi",
                                    ],
                                )
                                .add_filter("All files", &["*"])
                                .pick_file(),
                        )
                    },
                    Some(ui.ctx()),
                );
            }
        });
        if submitted && !self.image_path.trim().is_empty() {
            self.set_image(PathBuf::from(self.image_path.trim()));
        }
        match &self.image {
            None if !self.image_path.is_empty() => {
                ui.label("Analyzing…");
            }
            None => {
                ui.label("Select an image, type its path, or drop it here.");
            }
            Some(Err(error)) => {
                ui.colored_label(Color32::RED, error);
            }
            Some(Ok(report)) => {
                ui.label(format!("Type: {:?}", report.classification));
                ui.label(format!(
                    "Size: {}",
                    size_to_human_readable(report.image_size, SizeUnits::Fake)
                ));
                ui.label(format!("Bootable (DD): {:?}", report.is_bootable_img));
                if let Some(label) = &report.label {
                    ui.label(format!("Label: {label}"));
                }
                for warning in &report.warnings {
                    ui.colored_label(Color32::RED, warning);
                }
                if ui.button("Compute checksums").clicked() {
                    let path = PathBuf::from(&report.path);
                    self.spawn(
                        move || {
                            Message::Hash(
                                hash_file(
                                    &path,
                                    &HashAlgorithm::UPSTREAM_DEFAULT,
                                    &mut |_| {},
                                    &CancelToken::new(),
                                )
                                .map_err(|e| e.to_string()),
                            )
                        },
                        Some(ui.ctx()),
                    );
                }
            }
        }
        match &self.hashes {
            Some(Ok(report)) => {
                for digest in &report.digests {
                    ui.monospace(format!("{:<7} {}", digest.algorithm.name(), digest.hex));
                }
            }
            Some(Err(error)) => {
                ui.colored_label(Color32::RED, error);
            }
            None => {}
        }
    }

    /// The write modes available for the selected image (the first is the
    /// default: ISO mode for ISOs, as Rufus recommends).
    fn write_modes(&self) -> Vec<WriteMode> {
        match &self.image {
            Some(Ok(report)) => {
                let mut modes = Vec::new();
                if report.is_iso {
                    modes.push(WriteMode::Iso);
                }
                if report.is_bootable_img.is_dd_bootable() {
                    modes.push(WriteMode::Dd);
                }
                modes
            }
            _ => vec![WriteMode::Format],
        }
    }

    /// The options Rufus would offer for the selected device and image.
    pub fn format_options(&self) -> Option<FormatOptions> {
        let device = self.selected_device()?;
        let image = match &self.image {
            Some(Ok(report)) => Some(report),
            _ => None,
        };
        Some(format_options(
            &DeviceInput::of(device),
            image,
            &self.choices,
        ))
    }

    fn format_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Format Options");
        let Some(options) = self.format_options() else {
            ui.label("Select a device to see the format options.");
            return;
        };
        let mut changed = false;
        egui::ComboBox::new("partition-scheme", "Partition scheme")
            .selected_text(options.partition_scheme.map_or("", |p| p.label()))
            .show_ui(ui, |ui| {
                for scheme in &options.partition_schemes {
                    if ui
                        .selectable_label(options.partition_scheme == Some(*scheme), scheme.label())
                        .clicked()
                    {
                        self.choices.partition_scheme = Some(*scheme);
                        changed = true;
                    }
                }
            });
        egui::ComboBox::new("target-system", "Target system")
            .selected_text(options.target_system.map_or("", |t| t.label()))
            .show_ui(ui, |ui| {
                for target in &options.target_systems {
                    if ui
                        .selectable_label(options.target_system == Some(*target), target.label())
                        .clicked()
                    {
                        self.choices.target_system = Some(*target);
                        changed = true;
                    }
                }
            });
        let fs_label = options
            .file_system
            .and_then(|fs| options.file_systems.iter().find(|e| e.file_system == fs))
            .map_or(String::new(), |e| e.label.clone());
        egui::ComboBox::new("file-system", "File system")
            .selected_text(fs_label)
            .show_ui(ui, |ui| {
                for entry in &options.file_systems {
                    if ui
                        .selectable_label(
                            options.file_system == Some(entry.file_system),
                            &entry.label,
                        )
                        .clicked()
                    {
                        self.choices.file_system = Some(entry.file_system);
                        self.choices.cluster_size = None;
                        changed = true;
                    }
                }
            });
        let cluster_text = |size: u32| {
            if size == 0 {
                "Default".to_string()
            } else {
                format!("{} bytes", size)
            }
        };
        if let Some(entry) = options
            .file_system
            .and_then(|fs| options.file_systems.iter().find(|e| e.file_system == fs))
        {
            egui::ComboBox::new("cluster-size", "Cluster size")
                .selected_text(options.cluster_size.map_or(String::new(), cluster_text))
                .show_ui(ui, |ui| {
                    for size in &entry.cluster_sizes {
                        if ui
                            .selectable_label(
                                options.cluster_size == Some(*size),
                                cluster_text(*size),
                            )
                            .clicked()
                        {
                            self.choices.cluster_size = Some(*size);
                            changed = true;
                        }
                    }
                });
        }
        ui.horizontal(|ui| {
            let label = ui.label("Volume label");
            let mut text = options.label.clone();
            if ui
                .text_edit_singleline(&mut text)
                .labelled_by(label.id)
                .changed()
            {
                self.choices.label = Some(text);
                changed = true;
            }
        });
        for note in &options.notes {
            ui.label(RichText::new(&note.message).italics());
        }
        if changed {
            self.plan = None;
        }
    }

    fn plan_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Dry run");
        let device = self.selected_device().map(|d| d.facts.name.clone());
        let image = match &self.image {
            Some(Ok(report)) => Some(PathBuf::from(&report.path)),
            _ => None,
        };
        let modes = self.write_modes();
        let mode = self
            .mode
            .filter(|m| modes.contains(m))
            .or_else(|| modes.first().copied());
        if modes.len() > 1 {
            let label = |m: WriteMode| match m {
                WriteMode::Iso => "ISO image mode",
                WriteMode::Dd => "DD image mode",
                WriteMode::Format => "Format only",
            };
            egui::ComboBox::new("write-mode", "Write mode")
                .selected_text(mode.map_or("", label))
                .show_ui(ui, |ui| {
                    for m in &modes {
                        if ui.selectable_label(mode == Some(*m), label(*m)).clicked() {
                            self.mode = Some(*m);
                            self.plan = None;
                        }
                    }
                });
        }
        let ready = device.is_some() && mode.is_some();
        if ui
            .add_enabled(ready, egui::Button::new("Review plan (dry run)"))
            .clicked()
            && let (Some(device), Some(mode)) = (device, mode)
        {
            let root = self.root.clone();
            let mut options = ScanOptions::for_this_process(self.listing.clone());
            if self.options.sysroot.is_some() {
                options.program_paths.clear();
            }
            let request = PrepareRequest {
                image: if mode == WriteMode::Format {
                    None
                } else {
                    image
                },
                choices: self.choices.clone(),
                ..PrepareRequest::default()
            };
            self.spawn(
                move || {
                    Message::Plan(Box::new(
                        prepare(
                            &root,
                            &options,
                            &device,
                            mode,
                            &request,
                            PlanOptions::default(),
                        )
                        .map_err(|e| e.to_string()),
                    ))
                },
                Some(ui.ctx()),
            );
        }
        if !ready {
            ui.label("Select a device to review what writing or formatting would do.");
        }
        match &self.plan {
            Some(Ok(context)) => {
                let plan = &context.plan;
                ui.colored_label(Color32::RED, &plan.data_loss.summary);
                for part in &plan.layout {
                    ui.label(format!(
                        "New partition: {} ({})",
                        part.name,
                        size_to_human_readable(part.size, SizeUnits::Fake)
                    ));
                }
                for check in &plan.checks {
                    let (mark, color) = match check.status {
                        CheckStatus::Pass => ("ok", Color32::DARK_GREEN),
                        CheckStatus::Warn => ("warning", Color32::from_rgb(0xb0, 0x70, 0x00)),
                        CheckStatus::Fail => ("FAIL", Color32::RED),
                    };
                    ui.colored_label(color, format!("[{mark}] {}", check.message));
                }
                let verdict = if plan.blocked {
                    "This plan is blocked."
                } else {
                    "This plan passes its checks, but this build cannot execute it."
                };
                ui.label(RichText::new(verdict).strong());
            }
            Some(Err(error)) => {
                ui.colored_label(Color32::RED, error);
            }
            None => {}
        }
    }
}

impl eframe::App for FerrufusApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| self.show(ui));
    }
}

#[cfg(test)]
mod tests {
    use super::running_as_root;

    #[test]
    fn root_detection() {
        assert!(running_as_root("Name:\tx\nUid:\t0\t0\t0\t0\n"));
        assert!(running_as_root("Uid:\t1000\t0\t0\t0\n"));
        assert!(!running_as_root(
            "Uid:\t1000\t1000\t1000\t1000\nGid:\t0\t0\t0\t0\n"
        ));
        assert!(!running_as_root("Uid:\t10\t100\t1000\t0\n"));
        assert!(!running_as_root(""));
    }
}
