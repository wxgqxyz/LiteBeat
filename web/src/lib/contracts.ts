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

// 字段名与服务端 JSON 键一一对应（albums 投影不含 sort_title）。
export interface AlbumSummary {
  id: Id;
  title: string;
  album_artist: string;
}

// 单曲详情不含磁盘路径；size_bytes 服务端以字符串返回。
export interface TrackDetail extends TrackSummary {
  album_title: string;
  codec: string | null;
  mime: string | null;
  size_bytes: string | null;
}

export interface QueueIds {
  ids: Id[];
  truncated: boolean;
}
