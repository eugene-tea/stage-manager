//! The windows a restart brought back on another Desktop than their saved one, as the
//! restore's line ends with them: each one the pool placed in a part (`pool.rs`) is noted
//! with its saved Desktop, in the order they came back. Nothing else reads the list: macOS
//! gives apps no way to move a window to another Desktop, so the user is not told.

use super::{Model, WindowId, WindowRole};

impl Model {
    /// Notes that window `id` came back from saved Desktop `saved`'s entry on another Desktop.
    pub(crate) fn note_away(&mut self, id: WindowId, saved: u64) {
        self.away.retain(|(w, _)| *w != id);
        self.away.push((id, saved));
    }

    /// The noted windows still away, with their saved Desktop: managed, a standard window,
    /// not on their saved Desktop, in the order they came back.
    pub fn away_now(&self) -> impl Iterator<Item = (WindowId, u64)> + '_ {
        self.away.iter().copied().filter(|(id, saved)| {
            self.windows.get(id).is_some_and(|w| w.meta.role == WindowRole::Standard && w.space.0 != *saved)
        })
    }

    /// The windows still away as the restore's line ends with them, titles through `title`
    /// (a hash unless `[log] titles` is on): "; on another Desktop: com.example.ide #1a2b3c4d
    /// (saved 4, now 8), …". Empty when none is. The Desktops are the model's ids, as every
    /// log line has them.
    pub fn away_line(&self, title: impl Fn(&str) -> String) -> String {
        let items: Vec<String> = self
            .away_now()
            .filter_map(|(id, saved)| {
                let w = self.windows.get(&id)?;
                Some(format!("{} {} (saved {saved}, now {})", w.meta.bundle_id, title(&w.meta.title), w.space.0))
            })
            .collect();
        if items.is_empty() { String::new() } else { format!("; on another Desktop: {}", items.join(", ")) }
    }
}
