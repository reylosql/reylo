import browserCollections from 'collections/browser';
import type { PageFile } from '@/lib/content';

const loaded = new Map<string, Promise<PageFile>>();

/** A post's content as the browser loads it, a module of its own, fetched once. */
export function loadPost(path: string): Promise<PageFile> {
  let post = loaded.get(path);
  if (!post) {
    const load = browserCollections.blog.raw[`./${path}`];
    if (!load) throw new Error(`content/blog/${path} is not a post`);
    post = load();
    loaded.set(path, post);
  }
  return post;
}
