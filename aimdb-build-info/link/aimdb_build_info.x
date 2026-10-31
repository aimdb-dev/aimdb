/* aimdb-build-info: place the build-info section at AIMDB_BUILD_INFO_ADDR,
 * between the vector table and `.text`. The board's build script defines the
 * address with --defsym and moves `_stext` past the section, so the section's
 * address depends on neither its size nor the code's. */
SECTIONS
{
  .aimdb_build_info AIMDB_BUILD_INFO_ADDR :
  {
    KEEP(*(.aimdb_build_info));
  } > FLASH
}
INSERT AFTER .vector_table;

ASSERT(AIMDB_BUILD_INFO_ADDR >= ADDR(.vector_table) + SIZEOF(.vector_table),
  "aimdb-build-info: AIMDB_BUILD_INFO_ADDR overlaps the vector table");
ASSERT(AIMDB_BUILD_INFO_ADDR + SIZEOF(.aimdb_build_info) <= _stext,
  "aimdb-build-info: the section runs into .text; move _stext up");
ASSERT(SIZEOF(.aimdb_build_info) > 0,
  "aimdb-build-info: no section; call aimdb_build_info::section!() in the binary");
