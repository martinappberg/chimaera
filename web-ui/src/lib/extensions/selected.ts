/** One trusted literal assembly entry per window; absent builds never load private code. */
import { loadApplicationEntry } from "virtual:chimaera-application-entry";
import { installedExtension } from "./installed";
export const selectedApplication = loadApplicationEntry === null ? null : installedExtension(loadApplicationEntry);
