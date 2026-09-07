import sys
sys.path.insert(0, 'F:/coding/rs_h264')
from safeedit import edit
OLD = '''        for dy in 0..4 {
            let a = (row * 4 + dy) * w4 + b0;
            self.ref_idx_y[a..a + len].fill(g_r0);
            self.mv_y[a..a + len].fill(g_m0);
            self.ref_idx1[a..a + len].fill(g_r1);
            self.mv1[a..a + len].fill(g_m1);
            self.inter_y[a..a + len].fill(true);
            self.coded_y[a..a + len].fill(true);
            self.modes_y[a..a + len].fill(2);
            self.nnz_y[a..a + len].fill(0);
        }'''
NEW = '''        // ONE span per grid instead of one row-slice per grid PER ROW: the four
        // rows sit at a fixed stride, so a single `[base .. base + span]` covers
        // them and `chunks_mut` walks them with no further bounds arithmetic.
        // Eight range checks instead of thirty-two.
        let base = (row * 4) * w4 + b0;
        let span = 3 * w4 + len;
        macro_rules! fill4 {
            ($arr:expr, $v:expr) => {
                for ch in $arr[base..base + span].chunks_mut(w4) {
                    ch[..len].fill($v);
                }
            };
        }
        fill4!(self.ref_idx_y, g_r0);
        fill4!(self.mv_y, g_m0);
        fill4!(self.ref_idx1, g_r1);
        fill4!(self.mv1, g_m1);
        fill4!(self.inter_y, true);
        fill4!(self.coded_y, true);
        fill4!(self.modes_y, 2);
        fill4!(self.nnz_y, 0);'''
edit('crates/rusty_h264-decoder/src/mb16.rs', [(OLD, NEW, 'bz_flush_slow: one span per grid', 1)])
