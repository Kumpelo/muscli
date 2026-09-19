//! Album art for the current track and the album grid.
//!
//! Decoding happens on a worker thread; this module only decides what to
//! request and installs the results, keeping the bounded protocol cache in
//! step with what is on screen.

use super::*;

/// Everything the cover display owns.
///
/// Grouped because they are one mechanism: a request goes out, a decoded image
/// comes back, it lands in a bounded cache, and the placement signature decides
/// whether the next frame can be a partial repaint.
pub(super) struct Covers {
    /// The terminal graphics protocol chosen at startup.
    pub(super) picker: Picker,
    /// Art for the track being shown in the detail pane.
    pub(super) current: Option<CoverState>,
    /// Signature of the covers drawn in the last frame, and of the ones the
    /// next frame wants.
    ///
    /// Kitty images are anchored to text cells and ratatui only rewrites cells
    /// that changed, so a cover that moves leaves the old one smeared under the
    /// new. Comparing signatures between frames says when a full repaint is
    /// needed.
    pub(super) drawn_signature: u64,
    pub(super) pending_signature: u64,
    /// Decoded album-grid art, with the insertion order that bounds it.
    pub(super) grid: HashMap<PathBuf, StatefulProtocol>,
    pub(super) grid_order: VecDeque<PathBuf>,
    pub(super) requests: Sender<CoverDecodeRequest>,
    pub(super) results: tokio_mpsc::UnboundedReceiver<CoverDecodeResult>,
    /// Requests already sent, so the same image is not decoded twice.
    pub(super) in_flight: HashSet<(PathBuf, u32)>,
}

impl App {
    pub(super) fn refresh_cover(&mut self) {
        if !self.config.show_covers {
            self.covers.current = None;
            self.covers.grid.clear();
            self.covers.grid_order.clear();
            self.covers.in_flight.clear();
            return;
        }

        let path = self
            .detail_track()
            .and_then(|track| track.cover_path.clone());
        match path {
            Some(path)
                if self
                    .covers
                    .current
                    .as_ref()
                    .is_some_and(|cover| cover.path == path) => {}
            Some(path) => {
                self.covers.current = None;
                self.request_cover_decode(path, 512);
            }
            None => self.covers.current = None,
        }
    }

    pub(super) fn request_cover_decode(&mut self, path: PathBuf, size: u32) {
        let key = (path.clone(), size);
        if !self.covers.in_flight.insert(key.clone()) {
            return;
        }
        if self
            .covers
            .requests
            .send(CoverDecodeRequest { path, size })
            .is_err()
        {
            self.covers.in_flight.remove(&key);
        }
    }

    pub(super) fn handle_cover_decode_result(&mut self, result: CoverDecodeResult) {
        self.covers
            .in_flight
            .remove(&(result.path.clone(), result.size));
        let Some(image) = result.image else {
            return;
        };

        match result.size {
            512 => {
                let wanted = self
                    .detail_track()
                    .and_then(|track| track.cover_path.as_ref());
                if wanted.is_some_and(|path| path == &result.path) {
                    self.covers.current = Some(CoverState {
                        path: result.path,
                        protocol: self.covers.picker.new_resize_protocol(image),
                    });
                    self.dirty = true;
                }
            }
            256 => {
                self.covers.grid.insert(
                    result.path.clone(),
                    self.covers.picker.new_resize_protocol(image),
                );
                self.covers.grid_order.retain(|path| path != &result.path);
                self.covers.grid_order.push_back(result.path);
                self.dirty = true;
            }
            _ => {}
        }
    }
}
