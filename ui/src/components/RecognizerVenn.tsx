/**
 * The recognizer choice, drawn as what it actually is: a containment.
 *
 * A page reader covers a whole page — prose, and whatever else its `emits`
 * says it can read. A formula reader and a table reader each cover exactly one
 * kind, and only the areas the layout detector marked out as that kind. So the
 * three are not three alternatives in a list: two of them sit *inside* the
 * third's territory, and the question a user is actually asking — "will my
 * equations be read, and by what?" — is a question about which parts of one
 * box are filled.
 *
 * Hence the boxes. The outer box is everything a page reader is spent on; the
 * inner boxes are the kinds a specialist can take over. A region is painted in
 * the colour of the model that will actually read it, which makes the
 * precedence visible rather than documented: a page reader that emits formulas
 * paints the formula box only while no formula reader is installed, and the
 * moment one is, that box turns its colour and the page reader keeps the
 * padding around it — the prose, and every kind nothing else claims.
 *
 * Nothing here is keyed on a model id. Which box a specialist claims comes
 * from its own `emits`, and whether the page reader could have claimed it
 * comes from the page reader's. A fourth role is a fourth box, from the
 * catalogue, without an edit here.
 */

/** The little of a catalogue row this diagram reads. */
export interface VennModel {
  model_id: string;
  display_name: string;
  /** The region kinds this model can return. `snake_case`, as the backend
   *  serializes `RegionKind`. */
  emits: string[];
  is_cached: boolean;
  /** Chosen: this reader is meant to be spent on what it can read.
   *
   *  Separate from `is_cached` on purpose. Downloading a reader used to be
   *  the whole of choosing it, which left "I want my tables read this way"
   *  with no way to be said no — the only de-selection was deleting weights.
   *  A reader that is here and unchosen is a real state, and it is the state
   *  an empty box with a name in it is drawing. */
  selected: boolean;
}

/** One inner box: a kind, the name it is shown under, and the model whose job
 *  that kind is. */
export interface VennSpecialist {
  /** The region kind this reader owns, taken from its own `emits`. */
  kind: string;
  label: string;
  model: VennModel;
}

/** The outer box's own key, so a region is one string throughout. */
export const PAGE_REGION = "page";

interface Props {
  /** The page reader currently chosen in the picker, whose colour fills the
   *  padding. Null while the engine offers none. */
  page: VennModel | null;
  specialists: VennSpecialist[];
  /** Whether any of this is running at all. Nothing is painted while it is
   *  false, because nothing is read while it is false — the diagram draws the
   *  reading that happens, not the one that would if the feature were on. The
   *  checkboxes keep their own state through it, since switching the feature
   *  off is not a statement about which readers you want. */
  active: boolean;
  /** Which region's prose the panel is showing. */
  focus: string;
  onFocus: (region: string) => void;
  /** Select or de-select a region's reader. `PAGE_REGION` is the feature
   *  switch itself: there is no reading without a page reader, so choosing it
   *  and turning image analysis on are one act. */
  onToggle: (region: string, next: boolean) => void;
  disabled?: boolean;
}

/** Six hues that hold up against both themes' backgrounds at the alphas below.
 *  Deliberately not the app's accents: `--accent-blue` means "the control you
 *  are about to press" everywhere else, and a box painted with it would read
 *  as a button rather than as a model. */
const PALETTE = ["#6366f1", "#f59e0b", "#14b8a6", "#f43f5e", "#a855f7", "#84cc16"];

function hash(text: string): number {
  let value = 0;
  for (const character of text) {
    value = (value * 31 + character.codePointAt(0)!) >>> 0;
  }
  return value;
}

/**
 * A colour per model, stable across renders and distinct within one diagram.
 *
 * Stable because it is a hash of the id rather than a position in a list: a
 * model keeps its colour when another is installed beside it, and a legend
 * entry means the same thing before and after. Distinct because a collision
 * probes forward — two models the same colour would say they read the same
 * thing, which is the one claim this diagram exists to disprove.
 */
export function assignColours(models: VennModel[]): Record<string, string> {
  const taken = new Set<number>();
  const colours: Record<string, string> = {};
  for (const model of models) {
    if (colours[model.model_id]) continue;
    let slot = hash(model.model_id) % PALETTE.length;
    for (let probe = 0; probe < PALETTE.length && taken.has(slot); probe += 1) {
      slot = (slot + 1) % PALETTE.length;
    }
    taken.add(slot);
    colours[model.model_id] = PALETTE[slot];
  }
  return colours;
}

/** Why a box is empty, said rather than left to the outline. The three
 *  reasons cost the same reading and are undone by three different actions,
 *  so they must not collapse into one grey box. */
function absence(model: VennModel | null, active: boolean): string {
  if (!model) return "not read";
  if (!model.is_cached) return `${model.display_name} — not downloaded`;
  if (!model.selected) return `${model.display_name} — switched off`;
  if (!active) return `${model.display_name} — image analysis is off`;
  return model.display_name;
}

/** `#rrggbb` at an alpha, because the fills must let the panel's own
 *  background — and the text over them — through. */
function tint(hex: string, alpha: number): string {
  const value = parseInt(hex.slice(1), 16);
  const [r, g, b] = [(value >> 16) & 255, (value >> 8) & 255, value & 255];
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/**
 * Who reads this kind: the specialist if it is here, else the page reader if
 * it says it can, else nobody.
 *
 * The specialist wins whenever it is installed and not merely when the page
 * reader cannot — that is the whole precedence the extraction takes, and a
 * diagram that showed the page reader owning formulas while Texify sat on
 * disk would be showing a reading that does not happen.
 */
function readerOf(
  kind: string,
  page: VennModel | null,
  specialist: VennModel | null,
): VennModel | null {
  if (specialist?.is_cached && specialist.selected) return specialist;
  if (page?.emits.includes(kind)) return page;
  return specialist ?? null;
}

/**
 * A region's paint. Filled means read: a colour is only ever laid down for a
 * model that is on disk, chosen, and running, so "this box has a colour" and
 * "this kind comes back in the reading" are the same sentence.
 *
 * A kind whose reader is missing, de-selected, or switched off with the rest
 * of the feature is therefore an *empty* box, not a faint one — it still
 * names the model that would fill it, which is the useful half, without
 * claiming a reading that is not happening. Which of those three it is, the
 * box says in words; a colour cannot carry three states.
 */
function paint(
  model: VennModel | null,
  colours: Record<string, string>,
  active: boolean,
) {
  if (!active || !model?.is_cached || !model.selected) {
    return {
      background: "var(--bg-app)",
      borderColor: "var(--border-main)",
      borderStyle: "dashed" as const,
    };
  }
  const colour = colours[model.model_id];
  return {
    background: tint(colour, 0.34),
    borderColor: colour,
    borderStyle: "solid" as const,
  };
}

export default function RecognizerVenn({
  page,
  specialists,
  active,
  focus,
  onFocus,
  onToggle,
  disabled = false,
}: Props) {
  const colours = assignColours([
    ...(page ? [page] : []),
    ...specialists.map((entry) => entry.model),
  ]);

  const filled = specialists.map((entry) => ({
    ...entry,
    reader: readerOf(entry.kind, page, entry.model),
  }));

  // The padding, and nothing else: the kinds the page reader is left holding
  // once every chosen specialist has taken its own. Named rather than
  // implied, because "what is the big box actually doing now" is the question
  // the diagram is least able to answer by shape alone.
  const claimed = new Set(
    filled
      .filter((entry) => entry.reader && entry.reader.model_id !== page?.model_id)
      .map((entry) => entry.kind),
  );
  const remainder = (page?.emits ?? []).filter((kind) => !claimed.has(kind));

  const outer = paint(page, colours, active);
  const painted = active && !!page?.is_cached && !!page.selected;

  return (
    <div className="flex flex-col gap-2">
      <div
        className="rounded-xl border-2 p-2 transition-colors"
        style={outer}
        data-testid="venn-page"
      >
        <div className="flex items-center gap-2 px-1.5 py-1">
          {/* The page reader's own checkbox is the feature switch. There is no
              reading without a page reader — every area the detector marks out
              routes to one — so "use general OCR" and "read the text inside
              pictures" are the same question, and the backend refuses a
              configuration that answers them differently. One box, one
              setting, driven from either end. */}
          <input
            type="checkbox"
            aria-label="Use general OCR"
            checked={active}
            disabled={disabled || !page?.is_cached}
            onChange={(event) => onToggle(PAGE_REGION, event.target.checked)}
            className="h-3 w-3 shrink-0 accent-[var(--accent-blue)]"
          />
          <button
            type="button"
            disabled={disabled}
            onClick={() => onFocus(PAGE_REGION)}
            aria-pressed={focus === PAGE_REGION}
            className={`flex min-w-0 flex-1 items-baseline gap-2 rounded-md px-1 text-left transition-colors hover:bg-black/5 disabled:opacity-50 ${
              focus === PAGE_REGION ? "bg-black/10" : ""
            }`}
          >
            <span className="shrink-0 text-[10px] font-medium uppercase tracking-wider text-[var(--text-main)]">
              General OCR
            </span>
            <span className="truncate text-[10px] text-[var(--text-dim)]">
              {page ? absence(page, active) : "no page reader chosen"}
            </span>
          </button>
        </div>

        <div className="mb-1 px-1.5 text-[9px] italic text-[var(--text-dim)]">
          {page
            ? remainder.length > 0
              ? `${painted ? "Reads" : "Would read"} ${remainder.join(", ")} across the whole page.`
              : "Every kind it reads is taken by a reader below; it is left the prose."
            : "Nothing reads whole pages yet."}
        </div>

        <div
          className={`grid gap-2 ${specialists.length > 1 ? "grid-cols-2" : "grid-cols-1"}`}
        >
          {filled.map((entry) => {
            const inner = paint(entry.reader, colours, active);
            const focused = focus === entry.kind;
            return (
              <div
                key={entry.kind}
                data-testid={`venn-${entry.kind}`}
                className={`rounded-lg border-2 px-2 py-1.5 transition-colors ${
                  focused ? "ring-2 ring-[var(--accent-blue)]/60" : ""
                }`}
                style={inner}
              >
                <div className="flex items-center gap-1.5">
                  {/* De-selecting leaves the weights where they are. The kind
                      goes to the page reader if it can read it — which is why
                      this box does not simply empty when it is unchecked, and
                      why the checkbox is the specialist's rather than the
                      box's. */}
                  <input
                    type="checkbox"
                    aria-label={`Use ${entry.model.display_name} for ${entry.label.toLowerCase()}`}
                    checked={entry.model.selected}
                    disabled={disabled || !entry.model.is_cached}
                    onChange={(event) => onToggle(entry.kind, event.target.checked)}
                    className="h-3 w-3 shrink-0 accent-[var(--accent-blue)]"
                  />
                  <button
                    type="button"
                    disabled={disabled}
                    onClick={() => onFocus(entry.kind)}
                    aria-pressed={focused}
                    className="min-w-0 flex-1 rounded px-0.5 text-left transition-colors hover:bg-black/5 disabled:opacity-50"
                  >
                    <span className="block text-[10px] font-medium uppercase tracking-wider text-[var(--text-main)]">
                      {entry.label}
                    </span>
                    <span className="block truncate text-[9px] text-[var(--text-dim)]">
                      {absence(entry.reader, active)}
                    </span>
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      </div>

      {/* No colour key: every box already names the model that paints it,
          which is a better legend than a legend — it says *where* the colour
          applies. What a box cannot say is what an empty one means. */}
      <p className="px-1 text-[9px] italic text-[var(--text-dim)]">
        An empty, dashed box is a kind nothing reads; it names the model that
        would fill it and why it is not. Un-checking a reader keeps it
        downloaded — the kind it owned goes back to the page reader, if that
        one can read it at all.
      </p>
    </div>
  );
}
