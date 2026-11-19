//! The node's input margin reports (`0x25`, lockstep and sync rooms) turned into a change of the
//! lead the client stamps its inputs with.

/// Keeps one or two ticks of slack on the worst input of a report window.
#[derive(Debug, Default)]
pub struct MarginControl {
    slack_streak: u8,
    /// Smallest margin of the current streak of slack reports.
    spare: i8,
    /// Tick of the last report.
    report_tick: u32,
    /// Room tick by which every input stamped before the last raise has arrived.
    settle_tick: u32,
}

impl MarginControl {
    /// Ticks to add to the lead after a report (negative: to take off). A late report raises
    /// the lead by the lateness plus one tick, unless its window still covers inputs stamped
    /// before the last raise; three consecutive reports with more than two ticks to spare
    /// give the excess back. `last_stamped` is the tick of the newest input sent so far.
    pub fn adjust(&mut self, tick: u32, margin: i8, last_stamped: u32) -> i32 {
        let window_start = std::mem::replace(&mut self.report_tick, tick);
        if margin < 0 {
            self.slack_streak = 0;
            if window_start < self.settle_tick {
                return 0;
            }
            let lateness = u32::from(margin.unsigned_abs());
            self.settle_tick = last_stamped + lateness;
            return lateness as i32 + 1;
        }
        if margin <= 2 {
            self.slack_streak = 0;
            return 0;
        }
        self.spare = if self.slack_streak == 0 {
            margin
        } else {
            self.spare.min(margin)
        };
        self.slack_streak += 1;
        if self.slack_streak < 3 {
            return 0;
        }
        self.slack_streak = 0;
        2 - i32::from(self.spare)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_late_report_raises_once_and_slack_is_given_back() {
        let mut control = MarginControl::default();
        assert_eq!(control.adjust(30, -12, 100), 13);
        // the window of the next report began before the inputs stamped up to 100 had arrived
        assert_eq!(control.adjust(60, -12, 130), 0);
        assert_eq!(control.adjust(120, 1, 130), 0);
        assert_eq!(control.adjust(150, -1, 160), 2);
        assert_eq!(control.adjust(180, 2, 190), 0);
        assert_eq!(control.adjust(210, 9, 220), 0);
        assert_eq!(control.adjust(240, 6, 250), 0);
        assert_eq!(control.adjust(270, 7, 280), -4);
        // a tight report in between starts the count again
        assert_eq!(control.adjust(300, 5, 310), 0);
        assert_eq!(control.adjust(330, 1, 340), 0);
        assert_eq!(control.adjust(360, 5, 370), 0);
        assert_eq!(control.adjust(390, 5, 400), 0);
        assert_eq!(control.adjust(420, 5, 430), -3);
    }
}
