//! Full-window photo viewer: zoom with the wheel, drag to pan, ←/→ through
//! the photos of the chat, Esc to close. The photo is decoded at up to
//! 2560 px for this, once, and dropped when the viewer closes.

use iced::widget::{button, column, container, image, row, space, text};
use iced::{Element, Fill, Task};

use super::media::{FileState, Media};
use super::{App, Msg, WinId};

/// Largest side of the decoded picture (about 26 MB of pixels).
const MAX_SIDE: u32 = 2560;

pub(crate) struct PhotoView {
    pub(crate) window: WinId,
    pub(crate) file_id: i32,
    /// The decoded picture; `None` while downloading or decoding.
    pub(crate) image: Option<image::Handle>,
    pub(crate) error: Option<String>,
    /// Cancels the decode still running for a photo the viewer already
    /// stepped away from (dropped, and so aborted, when replaced or when
    /// the viewer closes).
    decode: Option<iced::task::Handle>,
}

/// Decodes a photo for the viewer, scaled to fit `MAX_SIDE`.
fn decode_full(path: &str) -> Result<(u32, u32, Vec<u8>), String> {
    let img = super::media::open_image(path)?;
    let img = if img.width().max(img.height()) > MAX_SIDE {
        img.resize(MAX_SIDE, MAX_SIDE, ::image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.into_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// The blocking closure owns the slot, not the abortable async waiter. Used
/// by the viewer decode path and by controlled worker-lifetime tests.
pub(crate) async fn decode_with_viewer_slot<T: Send + 'static>(
    permit_source: &'static tokio::sync::Semaphore,
    decode: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let permit = permit_source.acquire().await.map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decode()
    })
    .await
    .map_err(|e| e.to_string())
}

impl App {
    /// Photos of the chat shown in `window`, oldest first.
    fn chat_photos(&self, window: WinId) -> Vec<i32> {
        self.session
            .panes
            .get(&window)
            .map(|p| {
                p.messages
                    .iter()
                    .filter_map(|m| match &m.media {
                        Some(Media::Photo { file_id, .. }) => Some(*file_id),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn view_photo(&mut self, window: WinId, file_id: i32) -> Task<Msg> {
        self.session.photo_view = Some(PhotoView {
            window,
            file_id,
            image: None,
            error: None,
            decode: None,
        });
        let done = self.session.files.get(&file_id).is_some_and(|f| f.done);
        if done {
            self.decode_for_viewer(file_id)
        } else {
            // Also raised if it is already downloading at the bubble's
            // lower priority (16): TDLib reorders an in-progress download
            // when asked again with a higher one. An explicit action, so it
            // is also what retries a file that failed.
            self.request_download(file_id, 32)
        }
    }

    /// A download finished: the viewer may be waiting for it.
    pub(crate) fn viewer_file_done(&mut self, file_id: i32) -> Task<Msg> {
        let waiting = matches!(&self.session.photo_view, Some(v) if v.file_id == file_id && v.image.is_none());
        if waiting {
            self.decode_for_viewer(file_id)
        } else {
            Task::none()
        }
    }

    /// A download stopped without finishing (network error, no space, ...):
    /// shown as an error instead of an endless "Загрузка…".
    pub(crate) fn viewer_file_failed(&mut self, file_id: i32) -> Task<Msg> {
        if let Some(view) = &mut self.session.photo_view
            && view.file_id == file_id
            && view.image.is_none()
        {
            view.error = Some("не удалось загрузить фото".into());
        }
        Task::none()
    }

    /// Decodes the photo using a worker-held `DECODES` slot. The handle
    /// aborts the UI waiter on navigation, but a running native decode
    /// retains the slot until its blocking closure actually exits.
    fn decode_for_viewer(&mut self, file_id: i32) -> Task<Msg> {
        let Some(path) = self
            .session
            .files
            .get(&file_id)
            .map(|f: &FileState| f.path.clone())
        else {
            return Task::none();
        };
        let (task, handle) = Task::perform(
            async move {
                decode_with_viewer_slot(&super::media::DECODES, move || decode_full(&path))
                    .await
                    .and_then(|r| r)
            },
            move |r| Msg::ViewerDecoded(file_id, r),
        )
        .abortable();
        if let Some(view) = &mut self.session.photo_view {
            view.decode = Some(handle.abort_on_drop());
        }
        task
    }

    pub(crate) fn viewer_decoded(
        &mut self,
        file_id: i32,
        result: Result<(u32, u32, Vec<u8>), String>,
    ) {
        if let Some(view) = &mut self.session.photo_view
            && view.file_id == file_id
        {
            match result {
                Ok((w, h, rgba)) => view.image = Some(image::Handle::from_rgba(w, h, rgba)),
                Err(e) => view.error = Some(e),
            }
        }
    }

    /// ←/→: the previous or next photo of the chat.
    pub(crate) fn viewer_step(&mut self, forward: bool) -> Task<Msg> {
        let Some(view) = &self.session.photo_view else {
            return Task::none();
        };
        let (window, current) = (view.window, view.file_id);
        let photos = self.chat_photos(window);
        let Some(i) = photos.iter().position(|&f| f == current) else {
            return Task::none();
        };
        let next = if forward {
            i.checked_add(1)
        } else {
            i.checked_sub(1)
        };
        match next.and_then(|n| photos.get(n)) {
            Some(&file_id) => self.view_photo(window, file_id),
            None => Task::none(),
        }
    }

    /// The viewer over the whole window, if open in it.
    pub(crate) fn view_viewer(&self, window: WinId) -> Option<Element<'_, Msg>> {
        let view = self
            .session
            .photo_view
            .as_ref()
            .filter(|v| v.window == window)?;
        let photos = self.chat_photos(window);
        let position = photos
            .iter()
            .position(|&f| f == view.file_id)
            .map(|i| format!("{} / {}", i + 1, photos.len()))
            .unwrap_or_default();
        let picture: Element<'_, Msg> = match (&view.image, &view.error) {
            (Some(handle), _) => image::Viewer::new(handle.clone())
                .width(Fill)
                .height(Fill)
                .min_scale(0.25)
                .max_scale(8.0)
                .into(),
            (None, Some(e)) => container(text(e.clone()).style(text::danger))
                .center(Fill)
                .into(),
            // The bubble's picture while the big one loads.
            (None, None) => match self.session.images.peek(view.file_id) {
                Some(handle) => image(handle.clone()).width(Fill).height(Fill).into(),
                None => container(text("Загрузка…")).center(Fill).into(),
            },
        };
        let light = |b: iced::widget::Button<'static, Msg>| b.style(button::secondary);
        let bar = row![
            text(position).size(14).color(iced::Color::WHITE),
            space().width(Fill),
            light(button(text("Открыть в системе").size(13))).on_press(Msg::FileOpen(view.file_id)),
            light(button(text("Показать в папке").size(13)))
                .on_press(Msg::FileShowFolder(view.file_id)),
            light(button(text("×").size(16))).on_press(Msg::CloseViewer),
        ]
        .spacing(8)
        .align_y(iced::Center);
        let nav = |label: &'static str, forward: bool| {
            button(text(label).size(28).color(iced::Color::WHITE))
                .style(button::text)
                .on_press(Msg::ViewerStep(forward))
        };
        let screen = container(
            column![
                bar,
                row![nav("‹", false), picture, nav("›", true)].align_y(iced::Center),
            ]
            .spacing(8),
        )
        .padding(12)
        .width(Fill)
        .height(Fill)
        .style(|_| container::background(iced::Color::from_rgba8(0, 0, 0, 0.92)));
        // Clicks and scrolling stop here instead of reaching the chat below.
        Some(
            iced::widget::mouse_area(screen)
                .on_press(Msg::Ignore)
                .on_right_press(Msg::Ignore)
                .on_scroll(|_| Msg::Ignore)
                .into(),
        )
    }
}

#[cfg(test)]
pub(crate) fn decode_full_for_tests(path: &str) -> Result<(u32, u32, Vec<u8>), String> {
    decode_full(path)
}

#[cfg(test)]
mod tests {
    static TEST_DECODES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    #[test]
    fn big_photo_is_scaled_to_the_viewer_limit() {
        let path = std::env::temp_dir().join(format!("telega-viewer-{}.png", std::process::id()));
        ::image::RgbImage::new(3000, 1500).save(&path).unwrap();
        let (w, h, rgba) = super::decode_full(&path.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((w, h), (2560, 1280));
        assert_eq!(rgba.len(), (w * h * 4) as usize);
    }

    #[tokio::test]
    async fn security_fix_aborted_photo_waiters_keep_two_decoder_slots_until_workers_exit() {
        use std::sync::mpsc;
        use std::time::Duration;

        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut releases = Vec::new();
        let mut waiters = Vec::new();
        for id in 0..2 {
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let started_tx = started_tx.clone();
            waiters.push(tokio::spawn(async move {
                super::decode_with_viewer_slot(&TEST_DECODES, move || {
                    started_tx.send(id).unwrap();
                    // A native image decode cannot stop mid-call even if its
                    // viewer's async waiter is canceled by navigation.
                    let _ = release_rx.recv();
                    id
                })
                .await
            }));
            releases.push(release_tx);
        }
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
                .await
                .expect("blocking decoder starts")
                .expect("decoder reports its start");
        }

        for waiter in waiters {
            waiter.abort();
            assert!(waiter.await.unwrap_err().is_cancelled());
        }
        assert_eq!(
            TEST_DECODES.available_permits(),
            0,
            "navigation cannot free a running decoder's slot"
        );

        let (third_release_tx, third_release_rx) = mpsc::channel::<()>();
        let third_started = started_tx.clone();
        let third = tokio::spawn(async move {
            super::decode_with_viewer_slot(&TEST_DECODES, move || {
                third_started.send(2).unwrap();
                let _ = third_release_rx.recv();
                2
            })
            .await
        });
        assert!(
            started_rx.try_recv().is_err(),
            "a third decoder must wait while both canceled workers are running"
        );

        releases.remove(0).send(()).unwrap();
        let admitted = tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
            .await
            .expect("released worker opens one slot")
            .expect("next worker reports its start");
        assert_eq!(admitted, 2);
        assert_eq!(TEST_DECODES.available_permits(), 0);
        third_release_tx.send(()).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), third)
                .await
                .expect("replacement worker finishes")
                .unwrap()
                .unwrap(),
            2
        );
        releases.remove(0).send(()).unwrap();
        // Wait for both canceled blocking closures to finish, not just for
        // their discarded async results.
        let permits = tokio::time::timeout(Duration::from_secs(5), TEST_DECODES.acquire_many(2))
            .await
            .expect("canceled native workers eventually free both slots")
            .unwrap();
        drop(permits);
    }
}
