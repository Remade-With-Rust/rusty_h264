import sys
sys.path.insert(0, 'F:/coding/rs_h264')
from safeedit import edit

OLD = '''                    let mut avg = [0u8; 256];
                    rusty_h264_common::inter::avg_strided_into(
                        &ly0[s0..], l0, &ly1[s1..], l1, 16, 16, &mut avg,
                    );
                    for r in 0..16 {
                        let d = (mby * 16 + r) * self.cw + mbx * 16;
                        self.rec_y[d..d + 16].copy_from_slice(&avg[r * 16..r * 16 + 16]);
                    }'''
NEW = '''                    for r in 0..16 {
                        let d = (mby * 16 + r) * self.cw + mbx * 16;
                        rusty_h264_common::inter::avg_strided_into(
                            &ly0[s0 + r * l0..],
                            l0,
                            &ly1[s1 + r * l1..],
                            l1,
                            16,
                            1,
                            &mut self.rec_y[d..d + 16],
                        );
                    }'''
edit('crates/rusty_h264-decoder/src/mb16.rs', [(OLD, NEW, 'bi-average: per-row kernel, no staging', 1)],
     require_growth=False)
