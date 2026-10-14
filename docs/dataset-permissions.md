# Dataset permissions

Permissions given to us beyond a dataset's published licence, so that
`crates/chess-vision-train/datasets.toml` can rely on them. Each one is
recorded with who gave it, when, and what it asks of us.

## ChessReD

- **Dataset:** Chess Recognition Dataset (ChessReD), A. Masouris,
  4TU.ResearchData, doi:10.4121/99b5c721-280b-450b-b058-b2900b69a90f.
- **Published licence:** CC BY-NC-SA 4.0 (noncommercial).
- **Paper:** A. Masouris and J. van Gemert, "End-to-End Chess Recognition",
  VISAPP 2024, arXiv:2310.04086.
- **Permission:** granted by email on 14 October 2026 by Athanasios
  (Thanos) Masouris, the dataset's author and rights holder. Jan van Gemert
  (TU Delft Computer Vision Lab), his supervisor, wrote the same day that
  the rights lie with Thanos, who made the dataset as a student, and that he
  has no objection.

  > Your efforts, given the open-source nature of the project, seem
  > aligned. Therefore, consider this email as a confirmation that you're
  > granted permission to use ChessReD for commercial purposes, provided
  > that you will credit the dataset and our paper in your work.

- **Condition: credit the dataset and the paper.**
  - Every model trained on ChessReD carries the attribution line from
    `datasets.toml` (dataset and paper) in its model card.
  - Anywhere such a model is described or shipped (the Chess lab app's
    credits, release notes, docs) credits both too.
- **Licence id in `datasets.toml`:** `LicenseRef-chessred-permission-2026-10-14`.
  The original emails are kept by Mark Olliver.
