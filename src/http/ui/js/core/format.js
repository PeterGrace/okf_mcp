// Display formatting only; the server computes everything else.

export function ago(iso) {
    if (!iso) return '-';
    const seconds = Math.max(0, Math.round((Date.now() - Date.parse(iso)) / 1000));
    const steps = [[60, 's', 1], [3600, 'min', 60], [86400, 'h', 3600], [2592000, 'd', 86400]];
    for (const [limit, unit, size] of steps) if (seconds < limit) return `${Math.floor(seconds / size)} ${unit} ago`;
    return `${Math.floor(seconds / 2592000)} mo ago`;
}

export function when(iso) {
    return iso ? new Date(iso).toLocaleString() : '-';
}

export function count(n, noun) {
    return `${n} ${noun}${n === 1 ? '' : 's'}`;
}
