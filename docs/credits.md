# Credits

Chess lab and open-chess stand on other people's work. Thank you to all of
them.

## Reading boards from photos

The board reader learns from these datasets. Each trained model lists the
ones it was trained on in its model card, and the end of this page shows
the card of the reader that came with this app.

- **ChessReD**, the Chess Recognition Dataset: A. Masouris,
  4TU.ResearchData,
  [doi:10.4121/99b5c721-280b-450b-b058-b2900b69a90f](https://doi.org/10.4121/99b5c721-280b-450b-b058-b2900b69a90f).
  Also the paper: A. Masouris and J. van Gemert, *End-to-End Chess
  Recognition*, VISAPP 2024 ([arXiv:2310.04086](https://arxiv.org/abs/2310.04086)).
  Used with the author's permission ([dataset permissions](dataset-permissions.md)).
- **chesscog's rendered dataset**: G. Wölflein and O. Arandjelović,
  [doi:10.17605/OSF.IO/XF3KA](https://doi.org/10.17605/OSF.IO/XF3KA), CC BY 4.0.
- **Chess Board Dataset**: © 2019 Samuel Ryan, Mukund Venkateswaran, Kurt
  Convey and Michael Deng,
  [github.com/samryan18/chess-dataset](https://github.com/samryan18/chess-dataset), MIT.
- **robot-chess-commentator-squares** (boards): Felix Fabricius,
  [Hugging Face](https://huggingface.co/datasets/felixfabricius/robot-chess-commentator-squares), CC BY 4.0.
- **Annotated Image Dataset for Fenit**: Binus University, Mendeley Data,
  [doi:10.17632/wpp9xnbcp6.1](https://doi.org/10.17632/wpp9xnbcp6.1), CC BY 4.0.
- **Chess Pieces**: Roboflow Universe (joseph-nelson), CC0.
- **Our own rendered boards** ([chess-board-renderer](https://github.com/OffeneDatenmodellierung/chess-board-renderer)),
  set up from positions in the [Lichess open database](https://database.lichess.org) (CC0).
- **Photos sent to open-chess servers** by players who agreed to the
  [photo licence](photo-licence.md).

The reader's encoder starts from Meta's
[DINOv2](https://github.com/facebookresearch/dinov2) (Apache-2.0). Its
design follows the method published in Seytre's *ChessQueries: Toward
Better Chess Board Recognition*. It is written from the paper and uses
none of that project's code or weights.

## The rest of the app

- **The Atkinson Hyperlegible font**, by the Braille Institute, under the SIL
  Open Font License.
- **[Stockfish](https://stockfishchess.org)** (GPL-3.0) checks games for fair
  play on open-chess servers. It runs as a separate program and isn't part
  of this app.
- **[Lichess](https://lichess.org)**: open-chess speaks its API, so Lichess
  tools work with an open-chess server.

## The board reader in this app
