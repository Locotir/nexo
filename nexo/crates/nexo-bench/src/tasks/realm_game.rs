//! The source of the realm game, compiled by the task's setup with the system C compiler.
//!
//! It lives here, masked, rather than as a string the agent could find with `strings` in the nexo
//! binary: only the masked bytes reach the binary, and [`source`] unmasks them when the game is built.

/// The game, exactly as it is compiled.
const SOURCE: &str = r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct { char id[48], name[64], cls[16]; int level, hp, atk, bounty, gold; } Enemy;
typedef struct { char id[48], slot[16]; int weight, grd, lck, gld, atk, def, hp; } Item;
typedef struct { char id[48]; int tier, danger; char biome[16]; } Zone;
typedef struct { char name[48]; int level, luck, streak, n_eq; char eq[8][48]; } Save;

static Enemy E[400]; static int nE;
static Item I[400]; static int nI;
static Zone Z[64]; static int nZ;
static const int TIER_PCT[5] = {100, 120, 145, 175, 210};
static unsigned rng_state;

static unsigned rnd(void) { rng_state ^= rng_state << 13; rng_state ^= rng_state >> 17; rng_state ^= rng_state << 5; return rng_state; }

static int split(char *line, char **f, int max) {
  int n = 0; char *p = line;
  line[strcspn(line, "\r\n")] = 0;
  while (n < max) { f[n++] = p; p = strchr(p, '|'); if (!p) break; *p++ = 0; }
  return n;
}

static FILE *open_data(const char *name) {
  char path[256]; snprintf(path, sizeof path, "data/%s", name);
  FILE *fp = fopen(path, "r");
  if (!fp) { fprintf(stderr, "realm: cannot open %s (run from the game folder)\n", path); exit(2); }
  return fp;
}

static void parse_effects(Item *it, char *s) {
  char *tok = strtok(s, ";");
  while (tok) {
    char code[8] = {0}; int v = 0;
    if (sscanf(tok, "%7[A-Z]+%d", code, &v) == 2) {
      if (!strcmp(code, "GRD")) it->grd += v; else if (!strcmp(code, "LCK")) it->lck += v;
      else if (!strcmp(code, "GLD")) it->gld += v; else if (!strcmp(code, "ATK")) it->atk += v;
      else if (!strcmp(code, "DEF")) it->def += v; else if (!strcmp(code, "HP")) it->hp += v;
    }
    tok = strtok(NULL, ";");
  }
}

static void load(void) {
  char line[512]; char *f[10]; FILE *fp;
  fp = open_data("enemies.dat");
  while (fgets(line, sizeof line, fp)) {
    if (line[0] == '#' || split(line, f, 10) < 8) continue;
    Enemy *e = &E[nE++];
    strncpy(e->id, f[0], 47); strncpy(e->name, f[1], 63); strncpy(e->cls, f[2], 15);
    e->level = atoi(f[3]); e->hp = atoi(f[4]); e->atk = atoi(f[5]); e->bounty = atoi(f[6]); e->gold = atoi(f[7]);
  }
  fclose(fp);
  fp = open_data("items.dat");
  while (fgets(line, sizeof line, fp)) {
    if (line[0] == '#' || split(line, f, 10) < 4) continue;
    Item *it = &I[nI++]; memset(it, 0, sizeof *it);
    strncpy(it->id, f[0], 47); strncpy(it->slot, f[1], 15); it->weight = atoi(f[2]); parse_effects(it, f[3]);
  }
  fclose(fp);
  fp = open_data("zones.dat");
  while (fgets(line, sizeof line, fp)) {
    if (line[0] == '#' || split(line, f, 10) < 4) continue;
    Zone *z = &Z[nZ++];
    strncpy(z->id, f[0], 47); z->tier = atoi(f[1]); strncpy(z->biome, f[2], 15); z->danger = atoi(f[3]);
  }
  fclose(fp);
}

static Enemy *find_enemy(const char *id) { for (int i = 0; i < nE; i++) if (!strcmp(E[i].id, id)) return &E[i]; return NULL; }
static Item *find_item(const char *id) { for (int i = 0; i < nI; i++) if (!strcmp(I[i].id, id)) return &I[i]; return NULL; }
static Zone *find_zone(const char *id) { for (int i = 0; i < nZ; i++) if (!strcmp(Z[i].id, id)) return &Z[i]; return NULL; }

static void read_save(const char *path, Save *s) {
  FILE *fp = fopen(path, "r"); char line[1024];
  if (!fp) { fprintf(stderr, "realm: cannot open save %s\n", path); exit(2); }
  memset(s, 0, sizeof *s); s->level = 1;
  while (fgets(line, sizeof line, fp)) {
    line[strcspn(line, "\r\n")] = 0;
    char *eq = strchr(line, '='); if (!eq) continue; *eq++ = 0;
    if (!strcmp(line, "name")) strncpy(s->name, eq, 47);
    else if (!strcmp(line, "level")) s->level = atoi(eq);
    else if (!strcmp(line, "luck")) s->luck = atoi(eq);
    else if (!strcmp(line, "streak")) s->streak = atoi(eq);
    else if (!strcmp(line, "equipped")) {
      char *tok = strtok(eq, ",");
      while (tok && s->n_eq < 8) {
        if (!find_item(tok)) { fprintf(stderr, "realm: unknown item '%s' in save\n", tok); exit(2); }
        strncpy(s->eq[s->n_eq++], tok, 47); tok = strtok(NULL, ",");
      }
    }
  }
  fclose(fp);
  if (s->n_eq > 6) { fprintf(stderr, "realm: at most 6 items can be equipped\n"); exit(2); }
}

static int coins(const Save *s, const Enemy *e, const Zone *z) {
  int greed = 0, luck = s->luck;
  for (int i = 0; i < s->n_eq; i++) {
    int dup = 0;
    for (int j = 0; j < i; j++) if (!strcmp(s->eq[i], s->eq[j])) dup = 1;
    if (dup) continue;
    Item *it = find_item(s->eq[i]);
    greed += !strcmp(it->slot, "trinket") ? 2 * it->grd : it->grd;
    luck += it->lck;
  }
  int eff = luck <= 10 ? luck : 10 + (luck - 10) / 3;
  int diff = s->level - e->level, lvl = 100;
  if (diff > 4) { lvl = 100 - 15 * (diff - 4); if (lvl < 25) lvl = 25; }
  else if (diff < -3) { int k = -diff - 3; if (k > 5) k = 5; lvl = 100 + 10 * k; }
  int v = e->bounty * TIER_PCT[z->tier] / 100;
  v = v * lvl / 100;
  v = v * (100 + greed + 2 * eff) / 100;
  if (strcmp(e->cls, "critter")) v += 3 * (s->streak < 8 ? s->streak : 8);
  if (v < 1) v = 1;
  if (v > 999) v = 999;
  return v;
}

static const char *VERB[] = {"slashes", "bashes", "pierces", "scorches", "chills", "staggers", "grazes", "cleaves"};
static const char *FLAV[] = {"The air smells of iron.", "Dust rises from the floor.", "A distant bell rings.",
  "Torchlight flickers.", "Something skitters in the dark.", "Your boots slip on wet stone."};

static void duel(const Save *s, Enemy *e, Zone *z, unsigned seed) {
  int atk = 3 + s->level, def = s->level / 2, hp = 40 + 8 * s->level;
  for (int i = 0; i < s->n_eq; i++) { Item *it = find_item(s->eq[i]); atk += it->atk; def += it->def; hp += it->hp; }
  rng_state = seed ? seed : 0x9e3779b9u ^ (unsigned)(e->level * 7919 + s->level);
  int ehp = e->hp + z->danger * 3, round = 0;
  printf("== %s (lvl %d) enters %s [%s] ==\n", s->name[0] ? s->name : "Hero", s->level, z->id, z->biome);
  printf("A %s appears: %s, class %s, level %d, hp %d\n", e->id, e->name, e->cls, e->level, ehp);
  while (ehp > 0 && round < 60) {
    round++;
    unsigned r = rnd();
    int dmg = atk / 2 + (int)(r % (unsigned)(atk + 1));
    if (r % 7 == 0) { printf("[r%02d] you miss. rng=%08x\n", round, r); }
    else { ehp -= dmg; printf("[r%02d] you %s the %s for %d (enemy hp %d) rng=%08x\n", round, VERB[r % 8], e->id, dmg, ehp > 0 ? ehp : 0, r); }
    unsigned q = rnd();
    int edmg = e->atk - def / 3; if (edmg < 1) edmg = 1;
    if (q % 5 == 0) printf("[r%02d] the %s hesitates. %s\n", round, e->id, FLAV[q % 6]);
    else { hp -= edmg; if (hp < 1) { hp = 20 + s->level; printf("[r%02d] you fall... a shrine revives you (hp %d)\n", round, hp); }
           else printf("[r%02d] the %s %s you for %d (hp %d)\n", round, e->id, VERB[q % 8], edmg, hp); }
    if (q % 11 == 0) printf("[dbg] tick buffs=%d regen=0 seed=%08x\n", s->n_eq, q);
  }
  if (ehp > 0) printf("The %s retreats, wounded. You claim the field.\n", e->id);
  else printf("The %s is defeated after %d rounds.\n", e->id, round);
  printf("Loot: %d coins\n", coins(s, e, z));
}

static void usage(void) {
  puts("realm 0.9\n"
       "  realm duel --save FILE --enemy ID --zone ID [--seed N]   fight one enemy, print the log and the loot\n"
       "  realm sheet --save FILE                                   the character's base stats\n"
       "  realm help                                                this text\n"
       "Data is read from ./data (enemies.dat, items.dat, zones.dat).");
}

int main(int argc, char **argv) {
  if (argc < 2 || !strcmp(argv[1], "help") || !strcmp(argv[1], "--help")) { usage(); return 0; }
  const char *save = NULL, *enemy = NULL, *zone = NULL; unsigned seed = 0;
  for (int i = 2; i + 1 < argc; i += 2) {
    if (!strcmp(argv[i], "--save")) save = argv[i + 1];
    else if (!strcmp(argv[i], "--enemy")) enemy = argv[i + 1];
    else if (!strcmp(argv[i], "--zone")) zone = argv[i + 1];
    else if (!strcmp(argv[i], "--seed")) seed = (unsigned)strtoul(argv[i + 1], NULL, 10);
    else { fprintf(stderr, "realm: unknown option %s\n", argv[i]); return 2; }
  }
  load();
  if (!strcmp(argv[1], "sheet")) {
    if (!save) { usage(); return 2; }
    Save s; read_save(save, &s);
    printf("%s  level %d  luck %d  streak %d\nequipped:", s.name, s.level, s.luck, s.streak);
    for (int i = 0; i < s.n_eq; i++) printf(" %s", s.eq[i]);
    printf("\n");
    return 0;
  }
  if (!strcmp(argv[1], "duel")) {
    if (!save || !enemy || !zone) { usage(); return 2; }
    Save s; read_save(save, &s);
    Enemy *e = find_enemy(enemy); Zone *z = find_zone(zone);
    if (!e) { fprintf(stderr, "realm: unknown enemy '%s'\n", enemy); return 2; }
    if (!z) { fprintf(stderr, "realm: unknown zone '%s'\n", zone); return 2; }
    duel(&s, e, z, seed);
    return 0;
  }
  usage();
  return 2;
}
"#;

/// The mask applied to every byte; only needs to hide the text from a plain search of the binary.
const KEY: [u8; 7] = *b"realm09";

/// Masks or unmasks `src` (the operation is its own inverse).
const fn mask<const N: usize>(src: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = src[i] ^ KEY[i % KEY.len()];
        i += 1;
    }
    out
}

/// The only form of the source that ends up in the binary.
static MASKED: [u8; SOURCE.len()] = mask(SOURCE.as_bytes());

/// The C source of the game.
pub fn source() -> String {
    String::from_utf8(mask::<{ SOURCE.len() }>(&MASKED).to_vec()).expect("the game source is ASCII")
}
