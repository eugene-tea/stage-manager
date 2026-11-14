//! The inert-slivers tap's shield ([`crate::platform::corner::sliver_tap`], `[windows]
//! inert_slivers`): which windows sit in the park corner and where the corner's zones are,
//! told to the tap after every batch and whenever what a press there may reach changes.

use std::collections::HashSet;

use super::super::*;

/// The windows the sliver tap and the corner patch count parked: the model's parked windows
/// (`model`), and the windows whose restore failed while they are still listed in the park
/// zone (`unrestored`: id, in the zone at the last check). The model says shown the moment a
/// restore is asked; until the retry lands the sliver is there, and a press on it reached
/// the window at the park frame (the review's W4).
pub(in crate::platform::controller) fn shield_parked(model: impl IntoIterator<Item = u32>, unrestored: impl IntoIterator<Item = (u32, bool)>) -> HashSet<u32> {
    let mut parked: HashSet<u32> = model.into_iter().collect();
    parked.extend(unrestored.into_iter().filter(|(_, in_zone)| *in_zone).map(|(id, _)| id));
    parked
}

impl Controller {
    /// Tells the inert-slivers tap ([`crate::platform::corner::sliver_tap`], `[windows] inert_slivers`) the park
    /// corner's zone of each display with a parked window (the main display's always) and
    /// which windows sit parked (with incoming windows still held in the corner behind a
    /// transition's snapshots).
    ///
    /// Off while Mission Control, App Exposé or Show Desktop shows: their windows stand where
    /// the slivers are, and a press there is theirs. The Dock's bar comes from the cache
    /// [`Self::refresh_dock_bar`] keeps (no Accessibility call here: this runs after every
    /// batch).
    pub(in crate::platform::controller) fn publish_shield(&self) {
        let Some(m) = &self.model else { return };
        let on = self.store.config().windows.inert_slivers && self.overview.showing().is_none();
        let dock = if on { self.dock_bar.as_ref().map(|(_, b)| b.clone()).unwrap_or_default() } else { sliver::DockBar::default() };
        let mut parked: HashSet<u32> = if on {
            shield_parked(m.windows().filter(|w| w.parked).map(|w| w.meta.id.0), self.corner.unrestored.iter().map(|(id, u)| (*id, u.in_zone)))
        } else {
            HashSet::new()
        };
        if on && let Some(p) = &self.pending {
            parked.extend(p.entries.keys().copied());
        }
        let zones = if on {
            let mut used: Vec<DisplayId> = vec![m.main_display().id];
            for id in &parked {
                if let Some(w) = m.window(WindowId(*id)) {
                    let d = m.known_display(w.meta.display);
                    if !used.contains(&d) {
                        used.push(d);
                    }
                }
            }
            sliver::zones(used.into_iter().map(|id| {
                let d = m.display(id);
                (d.frame, d.visible, d.corner)
            }))
        } else {
            Vec::new()
        };
        crate::platform::corner::sliver_tap::publish(sliver::Shield { on, zones, parked, dock });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::unrestored::Unrestored;

    /// W4: a window whose restore failed is counted parked by the sliver tap and the patch
    /// while it is still listed in the park zone, though the model says shown.
    #[test]
    fn an_unrestored_window_still_in_the_zone_counts_as_parked_for_the_shield() {
        let parked = shield_parked([1, 2], [(3, true), (4, false)]);
        assert!(parked.contains(&3), "in the zone: its sliver is there");
        assert!(!parked.contains(&4), "seen out of the zone: shown");
        assert_eq!(parked.len(), 3);
        let u = Unrestored::failed(ParkedEntry { element: AxElement::system_wide(), restore: Rect::new(0.0, 0.0, 10.0, 10.0), child: false, fitted: None });
        assert!(u.in_zone, "a restore that just failed left the window where it was");
    }
}
