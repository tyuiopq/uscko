# Тестовые архивы

Используются тестами из `src/scenarios.rs` (`cargo test`).

RAR может создать только WinRAR, поэтому эти файлы лежат готовыми. Содержимое
всех архивов — папка `expected/` плюс пустая папка `пустая` (git пустые папки
не хранит, тесты добавляют её сами).

| Файл | Как сделан |
|---|---|
| `rar5.rar` | `rar a -r -ma5 rar5.rar .` |
| `volumes.part1..3.rar` | `rar a -r -ma5 -v20k volumes.rar .` — многотомный |
| `password.rar` | `rar a -r -ma5 -ppassword password.rar .` — пароль на файлы |
| `hidden-names.rar` | `rar a -r -ma5 -hppassword hidden-names.rar .` — пароль и на имена |
| `junction.rar` | `rar a -r -ma5 -ol junction.rar .` — файл `ok.txt` и точка соединения NTFS `escape` на чужую папку; распаковываться она не должна |
| `rar4.rar` | старый формат RAR 2.9; взят из тестов крейта [unrar](https://github.com/muja/unrar.rs) (MIT/Apache-2.0), внутри файл `VERSION` с текстом `unrar-0.4.0` |

`expected/данные.bin` — 50 000 случайных байт.
