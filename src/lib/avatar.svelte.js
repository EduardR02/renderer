/*
 * The account's picture, as it was last seen on the account's own profile
 * page, kept per account in localStorage. The rail's account row and Settings
 * draw it without asking the network for anything: until the profile has
 * been opened once they draw the letter, and afterwards the picture, which
 * the cover cache serves from disk.
 */
export const avatar = $state({ account: null, url: "" });

export function loadAvatar(account) {
  if (avatar.account === account) return;
  avatar.account = account || null;
  avatar.url = "";
  if (!account) return;
  try { avatar.url = localStorage.getItem(`sr.avatar:${account}`) ?? ""; }
  catch { /* Storage may be disabled; the letter stands in. */ }
}

export function rememberAvatar(account, url) {
  if (!account) return;
  const value = url || "";
  if (avatar.account === account) avatar.url = value;
  try {
    if (value) localStorage.setItem(`sr.avatar:${account}`, value);
    else localStorage.removeItem(`sr.avatar:${account}`);
  } catch { /* Storage may be disabled. */ }
}
