//! Album art for the current track and the album grid.
//!
//! Decoding happens on a worker thread; this module only decides what to
//! request and installs the results, keeping the bounded protocol cache in
//! step with what is on screen.

use super::*;

impl App {
    pub(super) fn refresh_cover(&mut self) {
        if !self.config.show_covers {
            self.cover = None;
            self.album_covers.clear();
            self.album_cover_order.clear();
            self.cover_decode_pending.clear();
            return;
        }

        let path = self
            .detail_track()
            .and_then(|track| track.cover_path.clone());
        match path {
            Some(path) if self.cover.as_ref().is_some_and(|cover| cover.path == path) => {}
            Some(path) => {
                self.cover = None;
                self.request_cover_decode(path, 512);
            }
            None => self.cover = None,
        }
    }

    pub(super) fn request_cover_decode(&mut self, path: PathBuf, size: u32) {
        let key = (path.clone(), size);
        if !self.cover_decode_pending.insert(key.clone()) {
            return;
        }
        if self
            .cover_decode_tx
            .send(CoverDecodeRequest { path, size })
            .is_err()
        {
            self.cover_decode_pending.remove(&key);
        }
    }

    pub(super) fn handle_cover_decode_result(&mut self, result: CoverDecodeResult) {
        self.cover_decode_pending
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
                    self.cover = Some(CoverState {
                        path: result.path,
                        protocol: self.picker.new_resize_protocol(image),
                    });
                    self.dirty = true;
                }
            }
            256 => {
                self.album_covers
                    .insert(result.path.clone(), self.picker.new_resize_protocol(image));
                self.album_cover_order.retain(|path| path != &result.path);
                self.album_cover_order.push_back(result.path);
                self.dirty = true;
            }
            _ => {}
        }
    }
}
