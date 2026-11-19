// The node's input margin reports (0x25, lockstep and sync rooms) turned into a change of the lead
// the client stamps its inputs with.
using System;

namespace Aurix.Net.Managed
{
    /// <summary>Keeps one or two ticks of slack on the worst input of a report window.</summary>
    public sealed class MarginControl
    {
        int _slackStreak;
        int _spare;
        uint _reportTick;
        uint _settleTick;

        /// <summary>Ticks to add to the lead after a report (negative: to take off). A late report raises the
        /// lead by the lateness plus one tick, unless its window still covers inputs stamped before the last
        /// raise; three consecutive reports with more than two ticks to spare give the excess back.
        /// <paramref name="lastStamped"/> is the tick of the newest input sent so far.</summary>
        public int Adjust(uint tick, sbyte margin, uint lastStamped)
        {
            uint windowStart = _reportTick;
            _reportTick = tick;
            if (margin < 0)
            {
                _slackStreak = 0;
                if (windowStart < _settleTick)
                {
                    return 0;
                }

                _settleTick = lastStamped + (uint)(-margin);
                return 1 - margin;
            }

            if (margin <= 2)
            {
                _slackStreak = 0;
                return 0;
            }

            _spare = _slackStreak == 0 ? margin : Math.Min(_spare, margin);
            if (++_slackStreak < 3)
            {
                return 0;
            }

            _slackStreak = 0;
            return 2 - _spare;
        }
    }
}
