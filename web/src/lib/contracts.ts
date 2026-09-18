export type Id = string;

export type PlaybackState =
  | 'idle'
  | 'loading'
  | 'playing'
  | 'paused'
  | 'buffering'
  | 'error';

export interface TrackSummary {
  id: Id;
  title: string;
  artist: string;
  album_id: Id | null;
  duration_ms: number | null;
  available: boolean;
}

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
  has_more: boolean;
}

export interface ApiError {
  error: { code: string; message: string; request_id: string };
}
