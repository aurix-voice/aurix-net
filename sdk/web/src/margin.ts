/** The node's input margin reports (`0x25`, lockstep and sync rooms) turned into a change of the
 *  lead the client stamps its inputs with: keeps one or two ticks of slack on the worst input of
 *  a report window. */
export class MarginControl {
  private slackStreak = 0;
  private spare = 0;
  private reportTick = 0;
  private settleTick = 0;

  /** Ticks to add to the lead after a report (negative: to take off). A late report raises the
   *  lead by the lateness plus one tick, unless its window still covers inputs stamped before the
   *  last raise; three consecutive reports with more than two ticks to spare give the excess back.
   *  `lastStamped` is the tick of the newest input sent so far. */
  adjust(tick: number, margin: number, lastStamped: number): number {
    const windowStart = this.reportTick;
    this.reportTick = tick;
    if (margin < 0) {
      this.slackStreak = 0;
      if (windowStart < this.settleTick) {
        return 0;
      }
      this.settleTick = lastStamped - margin;
      return 1 - margin;
    }
    if (margin <= 2) {
      this.slackStreak = 0;
      return 0;
    }
    this.spare = this.slackStreak === 0 ? margin : Math.min(this.spare, margin);
    if (++this.slackStreak < 3) {
      return 0;
    }
    this.slackStreak = 0;
    return 2 - this.spare;
  }
}
