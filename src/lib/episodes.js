const titleOrder = new Intl.Collator(undefined, { sensitivity: "base", numeric: true });

/** Order the complete browse result, not the rendered window. Unknown dates stay last. */
export function orderEpisodes(episodes, sort = "recent") {
  const ordered = [...episodes];
  if (sort === "title") {
    return ordered.sort((a, b) => titleOrder.compare(a.name ?? "", b.name ?? ""));
  }
  const direction = sort === "oldest" ? 1 : -1;
  return ordered.sort((a, b) => {
    const aKnown = Number.isFinite(a.published_at);
    const bKnown = Number.isFinite(b.published_at);
    if (aKnown !== bKnown) return aKnown ? -1 : 1;
    return aKnown ? direction * (a.published_at - b.published_at) : 0;
  });
}

/** Literal, case-insensitive title/description search; keep the chosen order. */
export function filterEpisodes(episodes, query) {
  const wanted = query.trim().toLowerCase();
  if (!wanted) return episodes;
  return episodes.filter((episode) =>
    String(episode.name ?? "").toLowerCase().includes(wanted)
    || String(episode.description ?? "").toLowerCase().includes(wanted),
  );
}
