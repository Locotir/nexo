# Curator defaults

The built-in curator starts from this specification, before user overrides. The
source of truth is `D_SPEC` in `crates/nexo-curator/src/params.rs`; a unit test
checks that this copy stays in sync. `segments=0` leaves experimental segments
disabled by default.

```text
threshold=0.02;head=turn;recall=1;link=0.5;rule=rent;birth=squeeze;eoq=16000;ledger=6000;notes=3000;rules=2000;pin=4000;protect=12;directives=0;nudge=0;close=2;ahead=0.25;margin=2.25;dedup=3;live=1;lead=1;aim=1;follow=1;brief=1;digest=1;hold=1;asked=3;fresh=1;still=32000;sup=1;named=64000;mission=1;apart=8000;check=4000;every=4;ratio=0.1;least=0.0005;floor=0.6;cap=0;target=10000;hard=0;cut=age;cards=8;stale=1;pockets=0;epoch=1;life=3600;kept=0;lcalls=12;lahead=0.2;lmargin=1;trim=0;need=0;relations=1;segments=0
```
