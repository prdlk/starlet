import { defineMeta } from "blume";

// The 4-digit filename prefixes already order the group; `pages` is omitted so a
// new ADR sorts into place by filename alone, with `index` first.
export default defineMeta({
  title: "Decisions",
  icon: "scale",
  order: 7,
});
